# Headless terminal & agent sessions (tmux-backed)

**Date:** 2026-07-24
**Status:** Design approved (pending spec review)
**Scope owner:** openOrchestra / harness-app

## Goal

Terminals and terminal-hosted agents must survive the app closing or updating,
and reconnect — still running, with history — when the app reopens. Closing or
updating the app should feel like detaching, not killing.

## Background: the current process model

The codebase's own principle (agent-runtime/lib.rs) is *"the substrate is the
terminal."* Running an agent means spawning its CLI (pi / claude / codex / kimi,
or opencode's TUI) as the command inside zed's `terminal` PTY. That PTY child is
a child of the GUI process, so app exit sends SIGHUP and it dies. Plain terminals
and terminal-hosted agents are therefore **one mechanism**: persist the PTY child
and they all survive together.

Two surfaces are explicitly **out of scope** (see Non-goals): the pi *chat*
sidecar (`pi --mode rpc`, a stdio-RPC child, not a TUI) and opencode's own server.

Detaching alone (setsid) is insufficient: the PTY master fd dies with the app, so
a surviving child cannot be reconnected. Recovery requires something to keep
owning the PTY. We use tmux for that.

## Scope

**In scope:** plain terminal tabs and terminal-hosted agent tabs
(pi / claude / codex / kimi / opencode TUI) survive app close/update and
reattach on reopen.

**Non-goals (this phase):**
- The pi **chat** tab (`pi --mode rpc`). Different problem (stdio RPC, in-flight
  turn state); revisit as a later phase.
- **opencode's** server model (it already keeps its own session store; the app
  only pumps its events). No change here.
- A from-scratch broker daemon. tmux is the daemon. A `session-broker` seam
  (Approach C) is noted as a future extension, not built now.

## Decisions (locked)

1. **Integration approach A — wrap at the terminal-command layer.** tmux is an
   implementation detail of how a pane's command is built. Nothing outside the
   terminal-pane path learns about tmux. (Rejected: baking tmux into
   `agent-runtime`'s `AgentCommand` — plain terminals need it too, and the
   headless crate should not know a UI-persistence concern; and a
   `session-broker` crate now — YAGNI.)
2. **Recovery: auto-reattach open tabs.** Persist which tabs were open per
   worktree; on launch, recreate exactly those tabs and reattach.
3. **Dead session on reopen → plain shell.** Reattach if the session is alive;
   if it is genuinely gone (reboot / tmux killed), restore an agent tab as a
   plain shell in the worktree — no unattended agent relaunch.
4. **Lifecycle asymmetry.** App close/update **detaches** (sessions live on);
   the user closing a tab (⌘W) **tears down** (`tmux kill-session`).

## Architecture

### Session model & naming

Each persistent tab maps to one tmux session named `harness_<slug>`, where the
slug derives from the worktree key and the tab's **stable id**, sanitized to
tmux-safe characters (tmux forbids `.` and `:` in session names; also strip
whitespace). The `harness_` prefix guarantees it never collides with the user's
own tmux sessions.

Stable ids are required because today's `Tab.id` is an ephemeral per-run counter.
We add a persisted, monotonic, never-reused `tab_id` (see Persistence).

### tmux command construction (Approach A)

`TerminalPane` already turns an `Option<AgentCommand>` into a zed `Shell`. It now
builds a `Shell` that runs a tmux **client**, with the agent/shell as the
session's root command:

- **Agent tab (fresh open):**
  `tmux new-session -A -s NAME -c CWD sh -lc '<script>'`
  where `<script>` sets the adapter env inline, runs the shell-quoted agent argv,
  then `exec "$SHELL" -l`. The `sh -lc 'agent; exec $SHELL'` wrapper preserves
  today's behavior — when the agent exits you drop into a live shell in the
  **same** session, instead of the session dying — and keeps the session alive
  for reattach.
- **Terminal tab (fresh open):** `tmux new-session -A -s NAME -c CWD` (tmux
  launches the default shell), env applied the same way.
- **Env** (e.g. `HARNESS_AGENT_SCHEMA`) is passed *inside* the `sh -lc` script
  (`K=V agent…`) rather than via `tmux -e`, to avoid a tmux-version dependency.
- Composing `<script>` requires shell-quoting the agent program + args and env
  values. This is a **pure function** (argv/env → script string) and is the
  primary unit-test seam.

The zed PTY runs only the tmux client; the tmux server (a daemon) owns the real
agent process.

### Reopen / restore flow

On launch, for each worktree in the registry, read its persisted tab list and
recreate those tabs. For each terminal/agent tab, the restore path is **not** a
blind `new-session -A` (that would re-run a dead agent, violating decision 3).
Instead:

1. Query live sessions once: `tmux ls -F '#{session_name}'`.
2. If the tab's session name is live → **attach** (`tmux attach -t NAME`, or
   `new-session -A` which attaches to the existing one).
3. If it is not live → open a **plain shell** session in the worktree (agent tabs
   do *not* relaunch their agent).

The alive-vs-dead classification given the parsed session set is a **pure
function** — the second unit-test seam.

Editor and chat tabs are recorded too; on restore they reopen **fresh** (an
editor rebuilds its hosted zed workspace; a chat tab respawns its `pi --mode rpc`
sidecar, exactly as opening one does today). Only terminal/agent tabs reattach a
live process.

### Lifecycle

- **App close / update.** The zed PTY runs only the tmux client. On app exit the
  client dies; the tmux server + agents keep running. No app-exit code path is
  needed — this is automatic.
- **User closes a tab (⌘W).** Explicit teardown: `tmux kill-session -t NAME`
  before dropping the pane. Closing means stop; only app-exit detaches.
- **Agent exits inside a session.** The `sh -lc 'agent; exec $SHELL'` wrapper
  drops the session to a login shell; the session stays alive and reattachable
  (this supersedes today's "respawn a fresh pane shell on CloseTerminal").

### tmux-absent fallback

Detect `tmux` on PATH at startup (store a bool alongside the existing
`detect_tools` check). If absent, `TerminalPane` uses today's direct-spawn path
unchanged, persistence/restore no-op, and the settings screen shows a one-line
hint ("install tmux for sessions that survive restarts"). Zero behavior change
for users without tmux.

## Components

1. **`crate::tmux` module (new).** Pure command-builder + a thin session query.
   - `session_name(worktree_key: &str, tab_id: u64) -> String` (sanitized).
   - `agent_script(cmd: &AgentCommand) -> String` — env + shell-quoted argv +
     `exec "$SHELL" -l`, the string passed to `sh -lc`.
   - `agent_shell(name, cwd, cmd) -> Shell` and `plain_shell(name, cwd) -> Shell`
     — the zed `Shell` (a `tmux` client invocation) the pane runs.
   - `live_sessions() -> Vec<String>` (runs `tmux ls`, parses; empty on error).
   - `is_live(name, &live_sessions) -> bool` — the pure reattach-vs-shell test.
   - `has_tmux() -> bool`.
   The process-spawning bits stay thin; the string/argv logic is pure.

2. **`TerminalPane` changes.** `new` gains a session identity (worktree key +
   stable tab id) and a mode (fresh vs restore). When tmux is present it builds
   the tmux-wrapped `Shell`; otherwise the current direct `Shell`. The
   `CloseTerminal` → respawn-plain-shell behavior is removed for tmux-backed
   panes (the in-session wrapper handles it).

3. **Panes-state persistence (new file).** `~/.openorchestra/panes.json`, same
   load/save pattern as `prefs`/`repos`:
   ```json
   {
     "next_tab_id": 12,
     "worktrees": {
       "<worktree_key>": {
         "active": 7,
         "tabs": [
           { "id": 7, "kind": "Agent", "agent": "pi", "title": "pi" },
           { "id": 9, "kind": "Terminal", "agent": null, "title": "shell" }
         ]
       }
     }
   }
   ```
   Written on tab open / close / activate. `next_tab_id` is a monotonic,
   never-reused counter; each new tab draws the next id.

4. **Stable tab ids.** `Tab` gains a persisted `id: u64` drawn from
   `next_tab_id`. (The existing ephemeral counter used for the active-tab pointer
   is replaced by this stable id.)

5. **Restore-on-launch.** After the worktree list is built, iterate persisted
   tabs and recreate them (which drives the reattach-vs-shell path per tab), and
   restore the active tab per worktree.

## Data flow

- **Open agent tab** → draw stable `tab_id` → record in panes.json → `TerminalPane`
  runs `tmux new-session -A -s harness_<wt>_<id> … sh -lc 'agent; exec $SHELL'`
  → attaches.
- **Close app / update** → tmux client dies; session + agent persist.
- **Reopen** → read panes.json → per terminal/agent tab, classify against
  `tmux ls` → attach (alive) or plain shell (dead); reopen editor/chat fresh;
  restore active tab.
- **Close tab (⌘W)** → `tmux kill-session -t NAME` → drop pane → update panes.json.

## Error handling & edge cases

- **tmux missing** → direct-spawn fallback (above).
- **`tmux ls` errors / no server** → treated as "no live sessions" (everything
  restores fresh; agent tabs → plain shell).
- **Session-name collision** → prevented by unique stable tab ids + `harness_`
  prefix; sanitization is deterministic.
- **Quoting** → agent argv and env values are shell-quoted when composing the
  `sh -lc` script; covered by unit tests.
- **Scrollback** → owned by tmux; the zed terminal shows the tmux client
  viewport, and reattach replays pane content. This is a deliberate,
  documented behavior change (scrollback is tmux copy-mode).
- **Resize** → the zed PTY (tmux client) drives size; tmux resizes the session on
  attach. Enable `aggressive-resize` if detached sizing needs it.

## Testing

**Pure unit seams (no GUI, no tmux):**
- `session_name` derivation & sanitization (forbidden chars, prefix, stability).
- tmux argv / `sh -lc` script construction: agent vs shell, env inlining, cwd,
  quoting of adversarial program/args/env.
- reattach-vs-shell decision given a parsed set of live session names.
- `panes.json` (de)serialization round-trip; `next_tab_id` monotonic and
  never-reused across open/close.

**Manual / integration:**
- Open an agent → quit app → reopen → same session, agent still running, history
  intact.
- Simulate reboot (`tmux kill-server`) → reopen → agent tab returns as a plain
  shell in the worktree.
- Close a tab (⌘W) → `tmux ls` no longer lists its session.
- Uninstall/PATH-hide tmux → app behaves exactly as today (no persistence).

## Future extensions (not now)

- **`session-broker` seam.** If a non-tmux backend is ever wanted (zero external
  dep), the `crate::tmux` module is the natural place to become a trait with tmux
  as the first impl.
- **pi chat persistence.** Requires pi to support a resumable/reconnectable RPC
  session, or running its sidecar under a daemon. Separate phase.
