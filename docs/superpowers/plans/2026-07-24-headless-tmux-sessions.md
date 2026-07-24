# Headless tmux-backed Sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make terminals and terminal-hosted agents survive the app closing/updating by running them inside tmux sessions the app attaches to, reattaching on reopen.

**Architecture:** Approach A — wrap at the terminal-command layer. `TerminalPane` runs a `tmux` client whose session command is the agent/shell; the tmux server (a daemon) owns the real process. A new pure `tmux` module builds the client argv and classifies live sessions; a `panes_state` module persists which tabs were open per worktree with stable ids; `ensure_pane` reattaches on (re)build.

**Tech Stack:** Rust, gpui, zed `terminal`/`util::shell::Shell`, serde/serde_json (already deps), tmux CLI (optional runtime dependency).

## Global Constraints

- Build/test harness-app from its **own directory**: `cd crates/harness-app && cargo <cmd>` (the root cargo silently skips it).
- tmux is an **optional** runtime dependency: if `tmux` is not on PATH, behave exactly as today (direct PTY spawn, no persistence). Never hard-fail on tmux absence.
- No new crate dependencies. Use `serde`/`serde_json` (already present). No `uuid` — stable tab ids come from a persisted monotonic `u64` counter.
- Session names are `harness_<sanitized>` — prefix `harness_`, and strip/replace any char that is not `[A-Za-z0-9_-]` (tmux forbids `.` and `:` and whitespace in session names).
- Agent env (e.g. `HARNESS_AGENT_SCHEMA`) is inlined into the `sh -lc` script (`K=V program …`), NOT passed via `tmux -e` or the zed terminal env (the agent runs in the tmux server's environment, not the client's).
- Keep zed's crate referenced as `::workspace` (this crate has its own `workspace` module).
- macOS is the target platform.

---

### Task 1: `tmux` module — pure command/classification helpers

**Files:**
- Create: `crates/harness-app/src/tmux.rs`
- Modify: `crates/harness-app/src/main.rs` (add `mod tmux;` near the other `mod` lines, ~line 19)
- Test: inline `#[cfg(test)] mod tests` in `crates/harness-app/src/tmux.rs`

**Interfaces:**
- Produces:
  - `pub fn session_name(worktree_key: &str, tab_id: u64) -> String`
  - `pub fn shell_quote(s: &str) -> String`
  - `pub fn agent_script(cmd: &agent_runtime::AgentCommand) -> String`
  - `pub fn client_args(session: &str, cwd: &std::path::Path, command: Option<&agent_runtime::AgentCommand>, attach_only: bool) -> Vec<String>`
  - `pub fn parse_sessions(ls_output: &str) -> Vec<String>`
  - `pub fn is_live(name: &str, live: &[String]) -> bool`

- [ ] **Step 1: Add the module declaration**

In `crates/harness-app/src/main.rs`, add `mod tmux;` alongside the existing module list (after `mod theme;` / before `mod workspace;`):

```rust
mod terminal_pane;
mod theme;
mod tmux;
mod workspace;
```

- [ ] **Step 2: Write the failing tests**

Create `crates/harness-app/src/tmux.rs` with only the test module and empty stubs so it compiles-and-fails:

```rust
//! tmux integration: pure builders for the client invocation and session
//! classification (this file), plus thin process wrappers (see `runtime`
//! submodule / Task 2). Kept pure so the argv, quoting, and reattach logic are
//! unit-testable without a tmux server or a GUI.

use std::path::Path;

use agent_runtime::AgentCommand;

/// Deterministic tmux session name for a tab. Prefixed so it never collides
/// with the user's own sessions; sanitized to tmux-legal characters.
pub fn session_name(worktree_key: &str, tab_id: u64) -> String {
    unimplemented!()
}

/// Single-quote a string for POSIX `sh`, escaping embedded single quotes.
pub fn shell_quote(s: &str) -> String {
    unimplemented!()
}

/// The script passed to `sh -lc`: inline env, the quoted agent argv, then drop
/// to a login shell so quitting the agent leaves a usable shell in the same
/// (still-persistent) session.
pub fn agent_script(cmd: &AgentCommand) -> String {
    unimplemented!()
}

/// Args after `tmux` for the client the pane runs.
/// - `attach_only` (restore of a live session): `new-session -A -s NAME`.
/// - otherwise: `new-session -A -s NAME -c CWD [sh -lc SCRIPT]` — with the
///   agent script when `command` is `Some`, a plain shell when `None`.
pub fn client_args(
    session: &str,
    cwd: &Path,
    command: Option<&AgentCommand>,
    attach_only: bool,
) -> Vec<String> {
    unimplemented!()
}

/// Parse `tmux ls -F '#{session_name}'` output into session names.
pub fn parse_sessions(ls_output: &str) -> Vec<String> {
    unimplemented!()
}

/// Whether `name` is among the live sessions.
pub fn is_live(name: &str, live: &[String]) -> bool {
    unimplemented!()
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

    #[test]
    fn client_args_fresh_agent() {
        let c = cmd();
        let a = client_args("harness_k_7", Path::new("/repo"), Some(&c), false);
        assert_eq!(a[0], "new-session");
        assert_eq!(a[1], "-A");
        assert_eq!(a[2], "-s");
        assert_eq!(a[3], "harness_k_7");
        assert!(a.iter().any(|x| x == "-c"), "cwd flag missing: {a:?}");
        assert!(a.iter().any(|x| x == "/repo"));
        assert_eq!(a[a.len() - 3], "sh");
        assert_eq!(a[a.len() - 2], "-lc");
        assert!(a.last().unwrap().contains("/usr/bin/pi"));
    }

    #[test]
    fn client_args_fresh_shell_has_no_command() {
        let a = client_args("harness_k_9", Path::new("/repo"), None, false);
        assert!(a.iter().any(|x| x == "-c"));
        assert!(!a.iter().any(|x| x == "sh"), "shell tab must have no sh -lc: {a:?}");
    }

    #[test]
    fn client_args_attach_only_is_bare_attach() {
        let a = client_args("harness_k_7", Path::new("/repo"), None, true);
        assert_eq!(a, vec!["new-session", "-A", "-s", "harness_k_7"]);
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
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cd crates/harness-app && cargo test --lib tmux:: 2>&1 | tail -20`
Expected: compiles, tests FAIL/panic with `not implemented`.

- [ ] **Step 4: Implement the pure helpers**

Replace each `unimplemented!()` body:

```rust
pub fn session_name(worktree_key: &str, tab_id: u64) -> String {
    let mut slug = String::with_capacity(worktree_key.len());
    for ch in worktree_key.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }
    format!("harness_{slug}_{tab_id}")
}

pub fn shell_quote(s: &str) -> String {
    // Wrap in single quotes; a literal ' becomes '\'' (close, escaped ', reopen).
    format!("'{}'", s.replace('\'', r"'\''"))
}

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

pub fn client_args(
    session: &str,
    cwd: &Path,
    command: Option<&AgentCommand>,
    attach_only: bool,
) -> Vec<String> {
    if attach_only {
        return vec![
            "new-session".into(),
            "-A".into(),
            "-s".into(),
            session.into(),
        ];
    }
    let mut args = vec![
        "new-session".into(),
        "-A".into(),
        "-s".into(),
        session.into(),
        "-c".into(),
        cwd.to_string_lossy().into_owned(),
    ];
    if let Some(cmd) = command {
        args.push("sh".into());
        args.push("-lc".into());
        args.push(agent_script(cmd));
    }
    args
}

pub fn parse_sessions(ls_output: &str) -> Vec<String> {
    ls_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn is_live(name: &str, live: &[String]) -> bool {
    live.iter().any(|s| s == name)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd crates/harness-app && cargo test --lib tmux:: 2>&1 | tail -20`
Expected: all tmux tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/tmux.rs crates/harness-app/src/main.rs
git commit -m "feat(tmux): pure session-name, quoting, client-argv, classify helpers"
```

---

### Task 2: `tmux` module — thin process wrappers

**Files:**
- Modify: `crates/harness-app/src/tmux.rs` (append runtime fns + one test)

**Interfaces:**
- Consumes: `parse_sessions` (Task 1).
- Produces:
  - `pub fn has_tmux() -> bool`
  - `pub fn live_sessions() -> Vec<String>`
  - `pub fn kill_session(name: &str)`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `crates/harness-app/src/tmux.rs`:

```rust
    #[test]
    fn has_tmux_matches_which() {
        let via_which = std::process::Command::new("which")
            .arg("tmux")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        assert_eq!(has_tmux(), via_which);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd crates/harness-app && cargo test --lib tmux::tests::has_tmux_matches_which 2>&1 | tail`
Expected: FAIL — `has_tmux` not defined.

- [ ] **Step 3: Implement the wrappers**

Append to `crates/harness-app/src/tmux.rs` (above `#[cfg(test)]`):

```rust
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

/// Tear down a session (used on explicit tab close). Best-effort.
pub fn kill_session(name: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", name]).output();
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cd crates/harness-app && cargo test --lib tmux:: 2>&1 | tail`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/tmux.rs
git commit -m "feat(tmux): has_tmux / live_sessions / kill_session process wrappers"
```

---

### Task 3: `panes_state` module — persistence types + load/save

**Files:**
- Create: `crates/harness-app/src/panes_state.rs`
- Modify: `crates/harness-app/src/main.rs` (add `mod panes_state;`)
- Test: inline `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `pub struct PanesState { pub next_tab_id: u64, pub worktrees: std::collections::BTreeMap<String, WorktreePanes> }`
  - `pub struct WorktreePanes { pub active: Option<u64>, pub tabs: Vec<TabRecord> }`
  - `pub struct TabRecord { pub id: u64, pub kind: TabRecordKind, pub title: String }`
  - `pub enum TabRecordKind { Shell, Agent(agent_runtime::AgentKind), Editor, Chat }`
  - `pub fn load(path: &std::path::Path) -> PanesState`
  - `pub fn save(path: &std::path::Path, state: &PanesState)`

- [ ] **Step 1: Add the module declaration**

In `crates/harness-app/src/main.rs`, add `mod panes_state;` with the others:

```rust
mod notifier;
mod panes_state;
mod shortcuts;
```

- [ ] **Step 2: Write the failing test**

Create `crates/harness-app/src/panes_state.rs`:

```rust
//! Persisted record of which tabs were open per worktree, so terminal/agent
//! tabs can be reattached to their tmux sessions on reopen. Same load/save
//! shape as prefs/repos (a JSON file under the app state dir).

use std::collections::BTreeMap;
use std::path::Path;

use agent_runtime::AgentKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PanesState {
    /// Monotonic, never-reused tab-id source. Stable ids let a tab keep the
    /// same tmux session name across restarts.
    #[serde(default)]
    pub next_tab_id: u64,
    #[serde(default)]
    pub worktrees: BTreeMap<String, WorktreePanes>,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorktreePanes {
    #[serde(default)]
    pub active: Option<u64>,
    #[serde(default)]
    pub tabs: Vec<TabRecord>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct TabRecord {
    pub id: u64,
    pub kind: TabRecordKind,
    pub title: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub enum TabRecordKind {
    Shell,
    Agent(AgentKind),
    Editor,
    Chat,
}

pub fn load(path: &Path) -> PanesState {
    unimplemented!()
}

pub fn save(path: &Path, state: &PanesState) {
    unimplemented!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");

        let mut state = PanesState { next_tab_id: 5, worktrees: BTreeMap::new() };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(3),
                tabs: vec![
                    TabRecord { id: 3, kind: TabRecordKind::Agent(AgentKind::Pi), title: "pi".into() },
                    TabRecord { id: 4, kind: TabRecordKind::Shell, title: "shell".into() },
                ],
            },
        );

        save(&path, &state);
        let back = load(&path);
        assert_eq!(back, state);
    }

    #[test]
    fn missing_file_is_default() {
        let got = load(Path::new("/no/such/panes.json"));
        assert_eq!(got, PanesState::default());
        assert_eq!(got.next_tab_id, 0);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd crates/harness-app && cargo test --lib panes_state:: 2>&1 | tail`
Expected: FAIL — `not implemented`.

- [ ] **Step 4: Implement load/save**

Replace the two bodies:

```rust
pub fn load(path: &Path) -> PanesState {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PanesState::default(),
    }
}

pub fn save(path: &Path, state: &PanesState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                log::warn!("could not write panes state to {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("could not serialize panes state: {e}"),
    }
}
```

- [ ] **Step 5: Run the test to verify it passes**

Run: `cd crates/harness-app && cargo test --lib panes_state:: 2>&1 | tail`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/panes_state.rs crates/harness-app/src/main.rs
git commit -m "feat(panes-state): persisted per-worktree tab records + load/save"
```

---

### Task 4: `TerminalPane` runs the tmux client when a session is given

**Files:**
- Modify: `crates/harness-app/src/terminal_pane.rs` (the `new` signature, `spawn`, and the `CloseTerminal` subscription)

**Interfaces:**
- Consumes: `tmux::client_args` (Task 1).
- Produces: `TerminalPane::new(worktree_root: PathBuf, agent: Option<AgentCommand>, session: Option<TerminalSession>, window, cx)` where
  `pub struct TerminalSession { pub name: String, pub attach_only: bool }`.

**Design:** When `session` is `Some`, the pane runs `tmux` with `client_args`; the agent env is inside the script, so the zed terminal env stays empty and the `CloseTerminal`→respawn behavior is disabled (the in-session `exec $SHELL` handles agent exit). When `session` is `None` (tmux absent), the current direct-spawn path is unchanged.

- [ ] **Step 1: Add the session struct and extend `new`**

At the top of `terminal_pane.rs`, add:

```rust
/// A tmux session this pane should run under. `attach_only` restores a live
/// session (bare `tmux attach`); otherwise the session is created/attached with
/// its command.
pub struct TerminalSession {
    pub name: String,
    pub attach_only: bool,
}
```

Change `pub fn new(...)` to thread `session: Option<TerminalSession>` into the struct and `spawn`:

```rust
pub fn new(
    worktree_root: PathBuf,
    agent: Option<AgentCommand>,
    session: Option<TerminalSession>,
    window: &mut Window,
    cx: &mut Context<Self>,
) -> Self {
    let window_id = window.window_handle().window_id().as_u64();
    let mut this = Self {
        view: None,
        spawned: String::new(),
        error: None,
        worktree_root,
        window_id,
        respawn_pending: false,
        tmux_backed: session.is_some(),
        focus_handle: cx.focus_handle(),
    };
    this.spawn(agent, session, window, cx);
    this
}
```

Add a `tmux_backed: bool` field to the `TerminalPane` struct definition.

- [ ] **Step 2: Rewrite `spawn` to build the tmux client shell when a session is given**

Replace the `let (shell, spawned, env) = match agent { ... };` block in `spawn` with:

```rust
fn spawn(
    &mut self,
    agent: Option<AgentCommand>,
    session: Option<TerminalSession>,
    window: &mut Window,
    cx: &mut Context<Self>,
) {
    let (shell, spawned, env) = match session {
        // tmux-backed: run a tmux client; the agent (and its env, inlined into
        // the script) live in the tmux server, so this pane's env is empty.
        Some(sess) => {
            let label = agent
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_else(|| "shell".to_string());
            let args = crate::tmux::client_args(
                &sess.name,
                &self.worktree_root,
                agent.as_ref().filter(|_| !sess.attach_only),
                sess.attach_only,
            );
            (
                Shell::WithArguments {
                    program: "tmux".to_string(),
                    args,
                    title_override: Some(label.clone()),
                },
                format!("{label} (tmux {})", sess.name),
                Vec::new(),
            )
        }
        // No tmux: today's direct spawn.
        None => match agent {
            Some(cmd) => {
                let spawned = format!("{} ({})", cmd.label, cmd.program.display());
                (
                    Shell::WithArguments {
                        program: cmd.program.to_string_lossy().into_owned(),
                        args: cmd.args,
                        title_override: Some(cmd.label),
                    },
                    spawned,
                    cmd.env,
                )
            }
            None => (Shell::System, "$SHELL".to_string(), Vec::new()),
        },
    };

    self.spawned = spawned;
    self.error = None;
    self.view = None;
    // ... unchanged: TerminalBuilder::new(...) through the end of spawn ...
}
```

(Leave the `TerminalBuilder::new(...)` call and the `cx.spawn_in` block below it unchanged.)

- [ ] **Step 3: Disable respawn-on-exit for tmux-backed panes**

In the `cx.subscribe(&terminal, ...)` closure inside `spawn`, guard the respawn:

```rust
cx.subscribe(&terminal, |pane, _term, event: &TerminalEvent, cx| {
    if matches!(event, TerminalEvent::CloseTerminal) && !pane.tmux_backed {
        pane.respawn_pending = true;
        cx.notify();
    }
})
.detach();
```

And in `render`, the `if self.respawn_pending` branch calls `self.spawn(None, None, window, cx)` (add the `None` session arg).

- [ ] **Step 4: Update the two call sites in `open_tab` to compile**

In `crates/harness-app/src/workspace/state.rs`, both `TerminalPane::new(r.path.clone(), None, window, cx)` and `TerminalPane::new(r.path.clone(), command, window, cx)` get a `None` third arg **for now** (real session wiring is Task 6):

```rust
cx.new(|cx| TerminalPane::new(r.path.clone(), None, None, window, cx)),
// ...
cx.new(|cx| TerminalPane::new(r.path.clone(), command, None, window, cx)),
```

- [ ] **Step 5: Compile and run existing tests**

Run: `cd crates/harness-app && cargo check 2>&1 | tail -5 && cargo test 2>&1 | tail -3`
Expected: compiles; `43 passed` (plus the new tmux/panes_state tests).

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/terminal_pane.rs crates/harness-app/src/workspace/state.rs
git commit -m "feat(terminal): run a tmux client when a session is provided"
```

---

### Task 5: Workspace state — tmux detection, global tab ids, persistence plumbing

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (struct fields ~line 400, constructor `new`, `PaneSet` definition ~line 331)
- Modify: `crates/harness-app/src/workspace/state.rs` (add `save_panes`)

**Interfaces:**
- Consumes: `panes_state::{PanesState, load, save, WorktreePanes, TabRecord, TabRecordKind}`, `tmux::has_tmux`.
- Produces on `HarnessWorkspace`:
  - fields `panes_state: panes_state::PanesState`, `panes_path: PathBuf`, `has_tmux: bool`
  - `pub(crate) fn next_tab_id(&mut self) -> u64` (draws + persists the counter)
  - `pub(crate) fn save_panes(&mut self)` (rebuilds `panes_state` from `self.panes` and writes)

- [ ] **Step 1: Add fields to the struct**

In `crates/harness-app/src/workspace.rs`, in the `HarnessWorkspace` struct (near `pane_clock`/`edit`), add:

```rust
    /// Persisted open-tab records for reattaching tmux sessions on reopen.
    panes_state: crate::panes_state::PanesState,
    panes_path: PathBuf,
    /// Whether `tmux` is available; when false, sessions are not persistent.
    has_tmux: bool,
```

- [ ] **Step 2: Remove `PaneSet::next_id`, use a global counter**

In the `PaneSet` struct (~line 331) delete the `next_id: u64` field and its doc line. Fix the initializer in `open_tab` (Task 6 rewrites that block; for now, in `workspace/state.rs`'s `open_tab`, the `PaneSet { tabs, active, next_id, last_used }` literal loses `next_id`).

- [ ] **Step 3: Initialize the new fields in `new`**

In `HarnessWorkspace::new` (workspace.rs), near where `prefs`/`data_dir` are set up, add:

```rust
        let panes_path = data_dir.join("panes.json");
        let panes_state = crate::panes_state::load(&panes_path);
        let has_tmux = crate::tmux::has_tmux();
```

and include `panes_state`, `panes_path`, `has_tmux` in the struct literal returned by `new`.

- [ ] **Step 4: Add `next_tab_id` and `save_panes` in `workspace/state.rs`**

Append these methods to the `impl HarnessWorkspace` block in `crates/harness-app/src/workspace/state.rs`:

```rust
    /// Draw the next globally-unique, persisted tab id.
    pub(crate) fn next_tab_id(&mut self) -> u64 {
        // next_tab_id starts at 0 on a fresh install; first id handed out is 1.
        self.panes_state.next_tab_id += 1;
        let id = self.panes_state.next_tab_id;
        crate::panes_state::save(&self.panes_path, &self.panes_state);
        id
    }

    /// Rebuild the persisted tab records from the live pane sets and write them.
    /// Only terminal/agent tabs are meaningful to reattach, but every tab is
    /// recorded so the strip comes back intact.
    pub(crate) fn save_panes(&mut self) {
        use crate::panes_state::{TabRecord, TabRecordKind, WorktreePanes};
        let mut worktrees = std::collections::BTreeMap::new();
        for (key, pane) in &self.panes {
            let tabs = pane
                .tabs
                .iter()
                .map(|t| TabRecord {
                    id: t.id,
                    kind: match (&t.content, &t.agent) {
                        (crate::TabContentTag::Editor, _) => TabRecordKind::Editor,
                        (crate::TabContentTag::Chat, _) => TabRecordKind::Chat,
                        (_, Some(a)) => TabRecordKind::Agent(a.clone()),
                        _ => TabRecordKind::Shell,
                    },
                    title: t.title.clone(),
                })
                .collect();
            worktrees.insert(
                key.clone(),
                WorktreePanes { active: Some(pane.active), tabs },
            );
        }
        self.panes_state.worktrees = worktrees;
        crate::panes_state::save(&self.panes_path, &self.panes_state);
    }
```

**Note on `TabContentTag`:** `TabContent` holds live entities and can't be matched by kind ergonomically here. Add a tiny helper method on `Tab` in `workspace.rs` instead:

```rust
impl Tab {
    fn kind_tag(&self) -> crate::panes_state::TabRecordKind {
        use crate::panes_state::TabRecordKind as K;
        match &self.content {
            TabContent::Editor(_) => K::Editor,
            TabContent::Chat(_) => K::Chat,
            TabContent::Terminal(_) => match &self.agent {
                Some(a) => K::Agent(a.clone()),
                None => K::Shell,
            },
        }
    }
}
```

Then `save_panes` uses `t.kind_tag()` instead of the `TabContentTag` match above:

```rust
                    kind: t.kind_tag(),
```

- [ ] **Step 5: Compile and test**

Run: `cd crates/harness-app && cargo check 2>&1 | tail -8 && cargo test 2>&1 | tail -3`
Expected: compiles; existing + new tests pass. (Unused-warning on `save_panes`/`next_tab_id` is fine until Task 6 wires them.)

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/workspace.rs crates/harness-app/src/workspace/state.rs
git commit -m "feat(workspace): tmux detection, global persisted tab ids, save_panes"
```

---

### Task 6: `open_tab` / `close_tab` — name sessions, persist, kill on close

**Files:**
- Modify: `crates/harness-app/src/workspace/state.rs` (`open_tab`, `close_tab`)

**Interfaces:**
- Consumes: `next_tab_id`, `save_panes`, `tmux::session_name`, `tmux::kill_session`, `TerminalSession`.

- [ ] **Step 1: Assign the stable id first and thread a session into terminal tabs**

Rewrite the id-allocation + terminal-construction in `open_tab`. Draw the id **before** building content (the terminal needs its session name):

```rust
    pub(crate) fn open_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if matches!(kind, TabKind::Chat) && r.agent_kind.is_none() {
            self.note = "agent chat needs a worktree".to_string();
            cx.notify();
            return;
        }

        let tab_id = self.next_tab_id();
        let session = self.has_tmux.then(|| crate::terminal_pane::TerminalSession {
            name: crate::tmux::session_name(&r.key, tab_id),
            attach_only: false,
        });

        let (content, title) = match kind {
            TabKind::Shell => (
                TabContent::Terminal(cx.new(|cx| {
                    crate::terminal_pane::TerminalPane::new(r.path.clone(), None, session, window, cx)
                })),
                "❯_ shell".to_string(),
            ),
            TabKind::Agent(ref agent) => {
                let command = self.agent_command_for(agent, &r);
                let label = command
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|| "shell".to_string());
                (
                    TabContent::Terminal(cx.new(|cx| {
                        crate::terminal_pane::TerminalPane::new(r.path.clone(), command, session, window, cx)
                    })),
                    label,
                )
            }
            // Editor / Chat arms unchanged.
            TabKind::Editor => { /* unchanged */ }
            TabKind::Chat => { /* unchanged */ }
        };
```

Note: `session` is moved into whichever terminal arm runs; Editor/Chat ignore it (they are not tmux-backed). Because `session` is used in at most one arm, no clone is needed; if the borrow checker complains, wrap creation of `session` per-arm instead.

- [ ] **Step 2: Use the drawn `tab_id` (not `pane.next_id`) and persist**

Replace the pane-insert/push tail of `open_tab`:

```rust
        let agent_of_kind = match &kind {
            TabKind::Agent(a) => Some(a.clone()),
            _ => None,
        };
        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self.panes.entry(r.key.clone()).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        pane.last_used = self.pane_clock;
        pane.tabs.push(Tab { id: tab_id, title, content, agent: agent_of_kind });
        pane.active = tab_id;
        self.focus_active_pane(window, cx);
        self.save_panes();
        cx.notify();
    }
```

- [ ] **Step 3: Kill the session and persist on close**

In `close_tab`, before `pane.tabs.remove(ix)`, capture and kill the session; persist after:

```rust
    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if self.has_tmux {
            // Explicit close tears the session down (only app-exit detaches).
            crate::tmux::kill_session(&crate::tmux::session_name(&r.key, id));
        }
        let Some(pane) = self.panes.get_mut(&r.key) else { return };
        let Some(ix) = pane.tabs.iter().position(|t| t.id == id) else { return };
        pane.tabs.remove(ix);
        if pane.active == id {
            pane.active = pane
                .tabs
                .get(ix.saturating_sub(1))
                .or_else(|| pane.tabs.last())
                .map(|t| t.id)
                .unwrap_or(0);
        }
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx); // saves panes itself
            return;
        }
        self.focus_active_pane(window, cx);
        self.save_panes();
        cx.notify();
    }
```

- [ ] **Step 4: Compile and test**

Run: `cd crates/harness-app && cargo check 2>&1 | tail -8 && cargo test 2>&1 | tail -3`
Expected: compiles; tests pass.

- [ ] **Step 5: Manual verification**

Run: `cd crates/harness-app && cargo run`
- Open an agent tab in a worktree. In another terminal: `tmux ls` shows `harness_<key>_<id>`.
- Quit the app (⌘Q). `tmux ls` still shows the session (agent still running).
- Close a tab with ⌘W. `tmux ls` no longer lists that session.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/workspace/state.rs
git commit -m "feat(tabs): tmux-backed sessions on open, kill-session on close, persist"
```

---

### Task 7: Reattach persisted tabs in `ensure_pane`

**Files:**
- Modify: `crates/harness-app/src/workspace/state.rs` (`ensure_pane`, add `restore_tabs`)

**Interfaces:**
- Consumes: `panes_state`, `tmux::{live_sessions, is_live, session_name}`, `TerminalSession`, `TabKind`.

**Design:** When a pane is first built for a worktree, if we have persisted tab records for it, recreate those tabs (reattach live sessions; dead → plain shell). Otherwise fall back to a fresh shell. Editor/Chat records reopen fresh via `open_tab`.

- [ ] **Step 1: Rewrite `ensure_pane` to restore when records exist**

```rust
    pub(crate) fn ensure_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        self.pane_clock = self.pane_clock.wrapping_add(1);
        if let Some(existing) = self.panes.get_mut(&r.key) {
            existing.last_used = self.pane_clock;
            return;
        }

        let restored = self.restore_tabs(&r.key, window, cx);
        if !restored {
            self.open_tab(TabKind::Shell, window, cx);
        }
        self.evict_stale_panes(&r.key);
    }

    /// Recreate a worktree's persisted tabs. Returns false if there was nothing
    /// to restore (caller opens a fresh shell). Live tmux sessions reattach;
    /// dead ones (reboot / killed) come back as a plain shell — never an
    /// unattended agent relaunch.
    fn restore_tabs(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use crate::panes_state::TabRecordKind as K;
        let Some(saved) = self.panes_state.worktrees.get(key) else { return false };
        if saved.tabs.is_empty() {
            return false;
        }
        let records: Vec<_> = saved
            .tabs
            .iter()
            .map(|t| (t.id, t.kind_clone(), t.title.clone()))
            .collect();
        let saved_active = saved.active;
        let live = if self.has_tmux { crate::tmux::live_sessions() } else { Vec::new() };

        for (id, kind, title) in records {
            match kind {
                K::Editor => self.open_tab(TabKind::Editor, window, cx),
                K::Chat => self.open_tab(TabKind::Chat, window, cx),
                K::Shell | K::Agent(_) => {
                    self.restore_terminal_tab(key, id, &kind, &title, &live, window, cx)
                }
            }
        }

        // Restore the active tab if it still exists (ids are stable).
        if let (Some(active), Some(pane)) = (saved_active, self.panes.get_mut(key))
            && pane.tabs.iter().any(|t| t.id == active)
        {
            pane.active = active;
            self.focus_active_pane(window, cx);
        }
        self.panes.contains_key(key)
    }
```

Add `kind_clone()` on `TabRecordKind` (it isn't `Copy`): in `panes_state.rs`, derive `Clone` on `TabRecordKind` and `TabRecord`, and use `.clone()` — replace `t.kind_clone()` with `t.kind.clone()`. (Simplest: add `#[derive(Clone)]` to both, drop the `kind_clone` idea.)

- [ ] **Step 2: Add `restore_terminal_tab` (reattach-or-shell, preserving the stable id)**

Terminal/agent tabs must keep their **original id** (so the session name matches). `open_tab` draws a new id, so restore builds the tab directly:

```rust
    fn restore_terminal_tab(
        &mut self,
        key: &str,
        id: u64,
        kind: &crate::panes_state::TabRecordKind,
        title: &str,
        live: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::panes_state::TabRecordKind as K;
        let Some(r) = self.resolve() else { return };
        let name = crate::tmux::session_name(key, id);
        let alive = self.has_tmux && crate::tmux::is_live(&name, live);

        // Dead agent session → plain shell (decision 3). Live → attach_only.
        let (command, agent) = match (kind, alive) {
            (K::Agent(a), false) => {
                // Restore as a shell: keep the agent mark off so it reads as a shell.
                (None, None::<AgentKind>)
            }
            (K::Agent(a), true) => (None, Some(a.clone())),
            _ => (None, None),
        };
        let _ = command; // agent env is only needed on fresh create, not restore

        let session = self.has_tmux.then(|| crate::terminal_pane::TerminalSession {
            name: name.clone(),
            attach_only: alive,
        });
        let content = TabContent::Terminal(cx.new(|cx| {
            crate::terminal_pane::TerminalPane::new(r.path.clone(), None, session, window, cx)
        }));

        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self.panes.entry(key.to_string()).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        pane.last_used = self.pane_clock;
        pane.tabs.push(Tab { id, title: title.to_string(), content, agent });
        pane.active = id;
    }
```

(Restoring passes `agent: None` command on purpose — a live session is reattached with `attach_only`, and a dead one is a plain shell. The agent mark is shown only when it reattached live.)

- [ ] **Step 3: Compile and test**

Run: `cd crates/harness-app && cargo check 2>&1 | tail -8 && cargo test 2>&1 | tail -3`
Expected: compiles; tests pass.

- [ ] **Step 4: Manual verification (the headline behavior)**

Run: `cd crates/harness-app && cargo run`
- Open an agent tab, let it run. Quit the app.
- Reopen (`cargo run`), select that worktree. The agent tab is back and **still running** (same output/history) — reattached.
- Then simulate a reboot: quit app, run `tmux kill-server`, reopen, select the worktree. The tab comes back as a **plain shell** (no agent relaunch).

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/workspace/state.rs crates/harness-app/src/panes_state.rs
git commit -m "feat(restore): reattach live tmux tabs on pane build, dead → plain shell"
```

---

### Task 8: tmux-absent hint in Settings

**Files:**
- Modify: `crates/harness-app/src/workspace/ui/settings.rs` (add a one-line hint when `!self.has_tmux`)

**Interfaces:**
- Consumes: `self.has_tmux`.

- [ ] **Step 1: Add the hint row**

In `render_settings` (ui/settings.rs), near the "Extras" section, add:

```rust
        if !self.has_tmux {
            body = body.child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("Install tmux for terminal/agent sessions that survive app restarts."),
            );
        }
```

(Match the surrounding `body = body.child(...)` builder style; place it wherever section spacing reads cleanly.)

- [ ] **Step 2: Compile and test**

Run: `cd crates/harness-app && cargo check 2>&1 | tail -5 && cargo test 2>&1 | tail -3`
Expected: compiles; tests pass.

- [ ] **Step 3: Manual verification**

Temporarily hide tmux (`PATH=/usr/bin cargo run` if tmux isn't in `/usr/bin`, or rename it) → Settings shows the hint, terminals still work with today's behavior. Restore PATH → hint gone, sessions persist.

- [ ] **Step 4: Commit**

```bash
git add crates/harness-app/src/workspace/ui/settings.rs
git commit -m "feat(settings): hint to install tmux for persistent sessions"
```

---

## Notes carried from the spec (behavior changes to expect)

- **Eviction is now non-destructive.** Dropping a pane over `MAX_LIVE_PANES` closes the tmux *client*, not the session — the agent keeps running in tmux and reattaches on reselect. (Previously eviction killed the agent.) Stopping an agent is now an explicit ⌘W (kill-session). This is consistent with the persistence goal; no code change needed, but call it out in the commit/PR.
- **Scrollback** is owned by tmux (copy-mode) for tmux-backed panes.
- **Agent exit** drops to a login shell inside the same session (via `exec "$SHELL" -l`) instead of respawning a fresh pane.

## Self-review

- **Spec coverage:** session model/naming (Task 1), tmux command construction + env inlining + quoting (Task 1), reopen/restore + reattach-vs-shell (Task 7), lifecycle (open Task 6, close Task 6 kill-session, app-exit automatic, agent-exit Task 4), tmux-absent fallback (Tasks 4–8), persistence + stable ids (Tasks 3, 5, 6), testing seams (Tasks 1–3 unit; 6–7 manual). Non-goals (chat sidecar, opencode, broker) untouched. ✓
- **Placeholder scan:** Editor/Chat arms marked "unchanged" reference existing code the implementer keeps verbatim (shown fully in the spec's current-code context); every new function has complete code. ✓
- **Type consistency:** `TerminalSession { name, attach_only }`, `client_args(session, cwd, command, attach_only)`, `TabRecordKind { Shell, Agent(AgentKind), Editor, Chat }`, `session_name(key, id)` used consistently across Tasks 1, 4, 6, 7. `TabRecordKind`/`TabRecord` derive `Clone` (Task 7 step 1). ✓
