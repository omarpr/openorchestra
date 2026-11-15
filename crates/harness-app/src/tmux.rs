//! tmux integration: pure builders for the client invocation and session
//! classification (this file), plus thin process wrappers (see `runtime`
//! submodule / Task 2). Kept pure so the argv, quoting, and reattach logic are
//! unit-testable without a tmux server or a GUI.

use std::path::Path;

use agent_runtime::{AgentCommand, AgentKind};
use worktrees::interactive_login_command;

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

/// The script the pane's login shell runs: inline env, the quoted agent argv,
/// then drop to a shell so quitting the agent leaves a usable shell in the same
/// (still-persistent) session.
///
/// Env goes on as a command prefix (`K=v prog args`) rather than a preceding
/// `export`, so these values win over anything the user's `.zshrc` sets for the
/// agent process without leaking into the shell left behind afterwards.
pub fn agent_command_line(cmd: &AgentCommand) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (k, v) in &cmd.env {
        parts.push(format!("{k}={}", shell_quote(v)));
    }
    parts.push(shell_quote(&cmd.program.to_string_lossy()));
    for arg in &cmd.args {
        parts.push(shell_quote(arg));
    }
    parts.join(" ")
}

/// [`agent_command_line`] plus the shell left behind when the agent exits. Used
/// on the tmux path, where the session must outlive the agent to stay
/// reattachable; the direct-spawn path respawns a shell in-process instead.
pub fn agent_script(cmd: &AgentCommand) -> String {
    // Run the agent, then drop to a login *interactive* shell so the session
    // survives the agent exiting and stays reattachable. `-i` is explicit
    // because it is what makes zsh read `.zshrc`: this shell does get a tty (so
    // it would be interactive anyway), but leaving that implicit is what hid
    // the missing-`.zshrc` bug everywhere else.
    format!("{}; exec \"$SHELL\" -li", agent_command_line(cmd))
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
///   `$SHELL -lic SCRIPT` agent command ride on the new-session.
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
        // The user's own shell, login + interactive, so the agent starts with
        // the environment their terminal would have given it (see
        // `worktrees::interactive_login_command`).
        let (program, shell_args) = interactive_login_command(&agent_script(cmd));
        args.push(program);
        args.extend(shell_args);
    }
    args.extend([
        ";".into(),
        "set-option".into(), "-t".into(), session.into(), "status".into(), "off".into(),
        ";".into(),
        "set-option".into(), "-t".into(), session.into(), "mouse".into(), "on".into(),
    ]);
    if let Some(cmd) = command {
        // Mark the session as an agent's, so a restart can tell a running
        // agent from a shell even when the launch line is a user override
        // whose start command names no known program (see
        // `missing_agent_keys`). `new-session -A` on an existing session
        // re-marks it, which is harmless.
        args.extend([
            ";".into(),
            "set-option".into(), "-t".into(), session.into(), AGENT_OPTION.into(), cmd.label.clone(),
        ]);
    }
    args.extend([
        ";".into(),
        "attach-session".into(), "-t".into(), session.into(),
    ]);
    args
}

/// Session user option naming the agent a session was started for.
pub const AGENT_OPTION: &str = "@oo_agent";

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
    let out = run_bounded(&["ls", "-F", "#{session_name}"], LIST_TIMEOUT);
    match out {
        Some(o) if o.status.success() => parse_sessions(&String::from_utf8_lossy(&o.stdout)),
        _ => Vec::new(),
    }
}

/// `(session_name, pane_pid)` for every pane the tmux server holds. Empty on
/// any error (no server, tmux missing) — the resource band then attributes
/// nothing to worktrees and shows the base-app total only.
pub fn list_session_panes() -> Vec<(String, u32)> {
    let out = run_bounded(&["list-panes", "-a", "-F", "#{session_name} #{pane_pid}"], LIST_TIMEOUT);
    match out {
        Some(o) if o.status.success() => {
            parse_session_panes(&String::from_utf8_lossy(&o.stdout))
        }
        _ => Vec::new(),
    }
}

/// `(session_name, pane_start_command)` for every pane the tmux server holds.
/// Tab-separated: a start command can contain spaces but never a tab of ours.
pub fn session_start_commands() -> Vec<(String, String)> {
    let out = run_bounded(&["list-panes", "-a", "-F", "#{session_name}\t#{pane_start_command}"], LIST_TIMEOUT);
    match out {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.split_once('\t'))
            .map(|(n, c)| (n.to_string(), c.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Tab ids of live sessions that belong to `worktree_key` but are not in
/// `known`, ascending. These are sessions the saved tab list lost track of
/// (e.g. another app instance overwrote it) whose agents are still running.
/// The suffix after the key's prefix must be all digits, so a key that is a
/// prefix of another key's slug never claims that key's sessions.
pub fn orphan_tab_ids(worktree_key: &str, live: &[String], known: &[u64]) -> Vec<u64> {
    let prefix = session_prefix(worktree_key);
    let mut ids: Vec<u64> = live
        .iter()
        .filter_map(|name| name.strip_prefix(&prefix))
        .filter(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
        .filter_map(|rest| rest.parse().ok())
        .filter(|id| !known.contains(id))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Which agent a session was started with, read from its start command
/// (`agent_script` quotes the program path: `'/…/claude' '--flag' …`). A plain
/// shell tab has an empty start command. `None` for shells and agents this
/// build cannot name; such a session still reattaches, as a shell tab.
pub fn agent_from_start_command(cmd: &str) -> Option<(AgentKind, &'static str)> {
    use AgentKind as K;
    const PROGRAMS: [(&str, K); 5] = [
        ("claude", K::ClaudeCode),
        ("codex", K::Codex),
        ("opencode", K::OpenCode),
        ("kimi", K::Kimi),
        ("pi", K::Pi),
    ];
    PROGRAMS.into_iter().find_map(|(prog, kind)| {
        let quoted_path = format!("/{prog}'");
        let quoted_bare = format!("'{prog}'");
        (cmd.contains(&quoted_path) || cmd.contains(&quoted_bare)).then_some((kind, prog))
    })
}

/// What [`missing_agent_keys`] needs to know about one tmux pane.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneInfo {
    pub session: String,
    /// Foreground process name (`zsh` once an agent has exited).
    pub current: String,
    /// Started for an agent: tagged with [`AGENT_OPTION`], or (older,
    /// untagged sessions) a start command naming a known agent program.
    pub agent_session: bool,
}

/// One entry per tmux pane. `None` when tmux could not be asked (no server, timed out), so callers can
/// tell "nothing is running" from "don't know" and not flag every agent as
/// stopped on a hiccup.
pub fn pane_commands() -> Option<Vec<PaneInfo>> {
    let out = run_bounded(
        &["list-panes", "-a", "-F", "#{session_name}\t#{pane_current_command}\t#{@oo_agent}\t#{pane_start_command}"],
        LIST_TIMEOUT,
    )?;
    if !out.status.success() {
        // No server at all is a real answer: nothing is running.
        let err = String::from_utf8_lossy(&out.stderr);
        return (err.contains("no server running") || err.contains("No such file"))
            .then(Vec::new);
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| {
                let mut it = l.splitn(4, '\t');
                let session = it.next()?.to_string();
                let current = it.next()?.to_string();
                let tag = it.next().unwrap_or("");
                let start = it.next().unwrap_or("");
                let agent_session = !tag.is_empty() || agent_from_start_command(start).is_some();
                Some(PaneInfo { session, current, agent_session })
            })
            .collect(),
    )
}

/// A login/interactive shell, i.e. what an agent session drops to when the
/// agent exits (`agent_script` ends in `exec "$SHELL" -li`).
pub fn is_shell(command: &str) -> bool {
    matches!(
        command.trim_start_matches('-'),
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "login"
    )
}

/// Which of `expected` pane keys have no agent running. A key has a running
/// agent when one of its sessions was started with an agent command and is
/// not back at its shell (the agent quit, leaving the session's shell).
pub fn missing_agent_keys(expected: &[String], panes: &[PaneInfo]) -> std::collections::BTreeSet<String> {
    expected
        .iter()
        .filter(|key| {
            let prefix = session_prefix(key);
            !panes.iter().any(|p| {
                p.session
                    .strip_prefix(&prefix)
                    .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
                    && p.agent_session
                    && !is_shell(&p.current)
            })
        })
        .cloned()
        .collect()
}

/// Tear down a session (used on explicit tab close). Best-effort.
pub fn kill_session(name: &str) {
    let _ = run_bounded(&["kill-session", "-t", name], LIST_TIMEOUT);
}

/// Run `tmux <args>` and wait at most `timeout` for it. A tmux client can
/// block indefinitely (one `send-keys` sat for minutes waiting on the server
/// and, being called on the UI thread, froze the whole app), so anything that
/// talks to tmux on a path the user is waiting on goes through here. On
/// timeout the client is killed and `None` returned. Output must be small: it
/// is read only after exit, so a client filling its pipe would time out.
pub fn run_bounded(args: &[&str], timeout: std::time::Duration) -> Option<std::process::Output> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = Command::new("tmux")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                log::warn!("tmux {} timed out after {timeout:?}", args.first().unwrap_or(&""));
                return None;
            }
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    Some(std::process::Output { status, stdout, stderr })
}

/// Listing/killing sessions: these run on the UI thread during restore.
const LIST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a `send-keys` may take before we give up on it.
const SEND_KEYS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Send a command line to a live session and press Enter. Best-effort and
/// bounded; callers on the UI thread should still run it off-thread. Enter is
/// skipped if the text did not go through, so a half-delivered line is never
/// submitted (and Enter alone never submits what the user was typing).
pub fn send_keys(session: &str, command: &str) -> bool {
    let typed = run_bounded(&["send-keys", "-t", session, "-l", command], SEND_KEYS_TIMEOUT)
        .is_some_and(|o| o.status.success());
    if !typed {
        log::warn!("send-keys to {session} failed or timed out; not pressing Enter");
        return false;
    }
    run_bounded(&["send-keys", "-t", session, "Enter"], SEND_KEYS_TIMEOUT)
        .is_some_and(|o| o.status.success())
}

/// Queue [`send_keys`] on a single background worker and return at once.
/// Never block the UI thread on tmux; one worker (not a thread per call) keeps
/// lines to the same session in order, so two messages cannot interleave.
pub fn send_keys_async(session: &str, command: &str) {
    use std::sync::{Mutex, OnceLock, mpsc};
    static WORKER: OnceLock<Mutex<mpsc::Sender<(String, String)>>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<(String, String)>();
        std::thread::Builder::new()
            .name("tmux-send-keys".into())
            .spawn(move || {
                for (session, command) in rx {
                    send_keys(&session, &command);
                }
            })
            .expect("spawn tmux send-keys worker");
        Mutex::new(tx)
    });
    if let Ok(tx) = tx.lock() {
        let _ = tx.send((session.to_string(), command.to_string()));
    }
}

/// Whether the session's active pane is in a tmux mode (copy mode, entered by
/// scrolling up or drag-selecting). `false` on any error.
pub fn pane_in_mode(session: &str) -> bool {
    run_bounded(
        &["display-message", "-p", "-t", session, "#{pane_in_mode}"],
        std::time::Duration::from_millis(500),
    )
    .is_some_and(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "1")
}

/// Leave copy mode if the pane is in it; a no-op otherwise.
pub fn cancel_copy_mode(session: &str) {
    let inner = format!("send-keys -X -t {session} cancel");
    let _ = run_bounded(
        &["if-shell", "-F", "-t", session, "#{pane_in_mode}", &inner],
        std::time::Duration::from_millis(500),
    );
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
        // `-li`, not `-l`: the replacement shell is only guaranteed to read
        // `.zshrc` if it is interactive, and relying on "stdin happens to be a
        // tty" is what made this class of bug invisible in the first place.
        assert!(s.trim_end().ends_with(r#"exec "$SHELL" -li"#), "{s}");
    }

    #[test]
    fn the_agent_runs_under_a_login_interactive_shell() {
        // The whole point: `sh -lc` never reads `.zshrc`, so the agent started
        // without the user's exports, aliases or version-manager shims — and
        // with a PATH that /etc/profile's path_helper had reordered.
        let c = cmd();
        let a = client_args("harness_k_7", Path::new("/repo"), Some(&c), false, &[]);
        let first_sep = a.iter().position(|x| x == ";").expect("command separator");
        let (program, args) = worktrees::interactive_login_command(&agent_script(&c));
        assert_eq!(a[first_sep - 3], program, "must be the user's shell, not sh: {a:?}");
        assert_eq!(a[first_sep - 2], args[0], "must be login + interactive: {a:?}");
        assert_ne!(a[first_sep - 3], "sh", "sh reorders PATH via path_helper");
        assert_ne!(a[first_sep - 2], "-lc", "-lc skips .zshrc");
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
        // No command at all: tmux then spawns `default-shell` itself, as a
        // login shell on a tty — already interactive, so it reads `.zshrc`.
        // Wrapping it would only add a redundant shell level.
        assert!(
            !a.iter().any(|x| x == "-lc" || x == "-lic"),
            "shell tab must not be wrapped in a `-c` shell: {a:?}"
        );
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

    #[test]
    fn orphan_ids_are_this_keys_unknown_sessions_only() {
        let key = "task:t:member:@coordinator";
        let live: Vec<String> = [
            "harness_task-t-member--coordinator_338",
            "harness_task-t-member--coordinator_339",
            "harness_task-t-member--coordinator_342",
            "harness_task-t-member--coordinator_x9",
            "harness_task-t-member--coordinator_extra_5",
            "harness_task-t-member-repo-t_341",
            "harness_main-repo_1",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(orphan_tab_ids(key, &live, &[339, 342]), vec![338]);
        assert!(orphan_tab_ids(key, &live, &[338, 339, 342]).is_empty());
    }

    #[test]
    fn agent_is_read_from_the_quoted_program_in_the_start_command() {
        let claude = r#"/bin/zsh -lic "HARNESS_TASK_CONTEXT='/x/agent-context.md' '/Users/u/.local/bin/claude' '--permission-mode' 'auto'; exec \"\$SHELL\" -li""#;
        assert_eq!(agent_from_start_command(claude).map(|a| a.0), Some(AgentKind::ClaudeCode));
        let pi = r#"/bin/zsh -lic "'/opt/bin/pi' '--mode' 'tui'; exec \"\$SHELL\" -li""#;
        assert_eq!(agent_from_start_command(pi).map(|a| a.0), Some(AgentKind::Pi));
        assert_eq!(agent_from_start_command(""), None);
        // A path that merely contains a program name is not that program.
        assert_eq!(agent_from_start_command("'/home/pilot/run.sh'"), None);
    }

    #[test]
    fn run_bounded_gives_up_on_a_hung_client() {
        if !has_tmux() {
            return;
        }
        // A private server, so the user's sessions are never touched.
        let sock = format!("oo-test-{}", std::process::id());
        let ok = run_bounded(
            &["-L", &sock, "new-session", "-d", "-s", "t", "sleep 30"],
            std::time::Duration::from_secs(5),
        )
        .is_some_and(|o| o.status.success());
        assert!(ok, "could not start a private tmux server");
        // `wait-for` blocks until signalled; nobody signals it.
        let start = std::time::Instant::now();
        let got = run_bounded(
            &["-L", &sock, "wait-for", "never"],
            std::time::Duration::from_millis(300),
        );
        let _ = run_bounded(&["-L", &sock, "kill-server"], std::time::Duration::from_secs(2));
        assert!(got.is_none(), "a hung tmux client must time out");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn an_agent_is_missing_unless_a_session_is_still_running_it() {
        let key = "task:t:member:repo/t".to_string();
        let row = |name: &str, cur: &str, agent: bool| PaneInfo {
            session: name.to_string(),
            current: cur.to_string(),
            agent_session: agent,
        };
        let expected = vec![key.clone()];
        // Running: an agent session whose foreground is the agent (Claude
        // shows its version as the process name).
        assert!(missing_agent_keys(&expected, &[row("harness_task-t-member-repo-t_7", "2.1.325", true)]).is_empty());
        // The agent quit: same session, back at its shell.
        assert_eq!(missing_agent_keys(&expected, &[row("harness_task-t-member-repo-t_7", "zsh", true)]).len(), 1);
        assert_eq!(missing_agent_keys(&expected, &[row("harness_task-t-member-repo-t_7", "-zsh", true)]).len(), 1);
        // Only a shell tab (e.g. after a reboot), even one running something.
        assert_eq!(missing_agent_keys(&expected, &[row("harness_task-t-member-repo-t_3", "cargo", false)]).len(), 1);
        // Another key's agent does not count.
        assert_eq!(missing_agent_keys(&expected, &[row("harness_task-t-member-other-t_7", "2.1.325", true)]).len(), 1);
        // Nothing at all.
        assert_eq!(missing_agent_keys(&expected, &[]).len(), 1);
    }

    #[test]
    fn agent_sessions_are_tagged_shell_sessions_are_not() {
        let tagged = |args: &[String]| args.windows(2).any(|w| w[0] == AGENT_OPTION && w[1] == "pi");
        let agent = client_args("s_1", Path::new("/tmp"), Some(&cmd()), false, &[]);
        assert!(tagged(&agent), "{agent:?}");
        // The tag is set before attaching (attach-session blocks until detach).
        let tag_at = agent.iter().position(|a| a == AGENT_OPTION).unwrap();
        let attach_at = agent.iter().position(|a| a == "attach-session").unwrap();
        assert!(tag_at < attach_at);
        let shell = client_args("s_2", Path::new("/tmp"), None, false, &[]);
        assert!(!shell.iter().any(|a| a == AGENT_OPTION));
    }

    /// Against a real tmux server on a private socket dir (never the user's):
    /// an agent session counts as running, then as missing once the agent
    /// exits back to the session's shell. Ignored by default because it
    /// changes TMUX_TMPDIR for the whole test process; run it alone with
    /// `cargo test live_tmux_missing_agent -- --ignored`.
    #[test]
    #[ignore]
    fn live_tmux_missing_agent_round_trip() {
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: this test runs alone (--ignored), so no other thread reads env.
        // TMUX must go too: inside a tmux pane it names the user's server and
        // wins over TMUX_TMPDIR.
        unsafe {
            std::env::remove_var("TMUX");
            std::env::set_var("TMUX_TMPDIR", dir.path());
        }
        // Refuse to touch anything unless we really are on a fresh, private
        // server: it must have no sessions at all yet.
        let pre = run_bounded(&["ls"], Duration::from_secs(2)).expect("tmux answered");
        assert!(
            !pre.status.success() && pre.stdout.is_empty(),
            "not isolated from the user's tmux server: {}",
            String::from_utf8_lossy(&pre.stdout)
        );
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        // A stand-in agent named like a real one. It exits cleanly (as an
        // agent does on /exit) once `quit` appears. Python, so its process
        // name is not a shell's while it runs.
        let quit = dir.path().join("quit");
        let fake = bin.join("claude");
        std::fs::write(
            &fake,
            format!(
                "#!/usr/bin/env python3\nimport os, time\nwhile not os.path.exists({:?}):\n    time.sleep(0.1)\n",
                quit.to_string_lossy()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();

        let key = "task:t:member:repo/t".to_string();
        let session = session_name(&key, 7);
        let cmd = AgentCommand { program: fake.clone(), args: vec![], env: vec![], label: "claude".into() };
        let mut args = client_args(&session, dir.path(), Some(&cmd), false, &[]);
        // Everything before attach-session (which needs a terminal).
        let attach = args.iter().position(|a| a == "attach-session").unwrap();
        args.truncate(attach - 1);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run_bounded(&argv, Duration::from_secs(5)).expect("tmux ran");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

        let wait_for = |want_missing: bool| {
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                let panes = pane_commands().expect("tmux answered");
                let missing = missing_agent_keys(std::slice::from_ref(&key), &panes);
                if missing.contains(&key) == want_missing {
                    return panes;
                }
                assert!(Instant::now() < deadline, "never became missing={want_missing}: {panes:?}");
                std::thread::sleep(Duration::from_millis(200));
            }
        };
        let running = wait_for(false);
        assert!(running.iter().any(|p| p.session == session && p.agent_session), "{running:?}");

        // The agent quits: the session falls back to its login shell.
        std::fs::write(&quit, "").unwrap();
        let after = wait_for(true);
        assert!(after.iter().any(|p| p.session == session && is_shell(&p.current)), "{after:?}");

        let _ = run_bounded(&["kill-server"], Duration::from_secs(2));
    }
}
