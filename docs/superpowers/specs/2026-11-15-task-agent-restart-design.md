# Restart a missing task agent

Date: 2026-11-15 · Status: proposed · Branch: `feat/task-coordinator`

## Why

A multi-project task is a coordinator agent plus one agent per repo, wired
together over A2A. When one of those agents disappears, whether from a computer
restart (tmux dies), the agent quitting, or a tab closed by mistake, the
task silently loses a participant. Today, after a reboot, a dead agent tab comes
back as a plain shell. Nothing says the agent is gone, and getting it back means
knowing which agent, flags and context file to launch by hand. Completions sent
while it was gone are lost, because queued A2A lines live only in memory.

The user wants a missing agent to be obvious and one click to bring back,
with its conversation intact.

## Goals

- A task row (the coordinator) or repo row with no running agent says so, and
  offers **Restart**.
- Restart relaunches the same agent kind in the same folder with the same task
  context, **continuing its most recent conversation** where the CLI supports it.
- After the coordinator restarts, it re-checks its delegations, so results that
  arrived while it was gone are not lost.
- Closing a task agent's tab asks first, and the agent stays restartable.

## Non-goals

- Automatic relaunch with no click. This keeps the existing rule: never start
  an agent unattended. An agent in auto mode would start acting the moment a
  task is clicked.
- Locking agent tabs so they can't be closed. It doesn't survive a reboot, and
  it removes the way out of a stuck agent.
- A durable A2A delivery queue. The coordinator's re-check covers the loss.
- Recovering agents of plain (non-task) worktrees.

## Anti-goals

- Restart must never land in a *different* conversation than the one the agent
  had. A resumed agent in the wrong context is worse than a fresh one.
- Restart must never create a second live agent next to a running one.

## Approaches considered

1. **Lock agent tabs (no close).** Rejected: a reboot still kills them.
2. **Detect missing, offer Restart with resume (chosen).** One click,
   conversation kept, and no agent starts without the user asking.
3. **Auto-restart on open.** Rejected: reverses the "never unattended" rule.

## Design

**"Missing" means:** the pane key (`task:{id}:member:{wt}` or the
coordinator's key) has no live tmux session whose foreground process is the
agent, where the agent tab is not just a shell left behind after the agent
exited. The check is a background tmux query using the bounded runner
(`tmux::run_bounded`, never on the UI thread). It is refreshed when the sidebar
polls status and when a task is opened. Without tmux, "missing" means no agent
tab exists in the pane.

**Sidebar:** in place of the status pill, a missing agent's row shows a muted
`STOPPED` pill. Hovering shows "Agent not running — click to restart". A
**Restart** item also appears in the row's right-click menu. Clicking the pill
restarts.

**Restart:**
- Uses the kind the task records (`agent_kind`, as today's coordinator launch
  does) and the existing `agent_command_for` path, so hooks, the status watch and
  the task env/context file are identical to a first launch.
- Adds a per-kind **continue** argument. This is a new `AgentRuntime` method,
  `continue_args() -> Option<Vec<String>>`, defaulting to `None`:
  - Claude Code: `--continue` (verified: "most recent conversation in the
    current directory").
  - pi: `--continue` (verified: "most recent session for the current
    directory").
  - OpenCode: `--continue` (verified: "Continue the last session").
    **Builder:** confirm it is scoped to the current directory. If it isn't,
    return `None`.
  - Codex: `resume --last`. **Builder:** confirm it is limited to the current
    directory. If it is global, return `None`, because a wrong conversation
    breaks the anti-goal.
  - Kimi and custom agents: `None` (fresh start).
- Each agent's folder is unique: the coordinator's is the task folder and each
  repo's is its own worktree. So "most recent in this directory" is that
  agent's own conversation.
- Opens in a new agent tab in that pane, and makes it the active tab, so
  A2A delivers to it.
- If the CLI exits right away (no prior conversation to continue), the session
  drops to its shell as today. The row stays `STOPPED`, and its tooltip offers
  "Start fresh", which launches without the continue argument.

**Coordinator re-check:** after a coordinator restart, once its session is
live, the app sends one line through the existing A2A injection path
(`send_keys_async`): `[openOrchestra] You were restarted. Check the status of
every delegation you have open (./a2a/status.sh <id>) and report anything that
finished while you were away.` Members get no such line, because their work
items are re-sent by the coordinator if needed.

**Close confirmation:** closing the agent tab of a task pane with a live
agent asks "Stop the <repo> agent for <task>?". Stop or Cancel. A stopped agent
is not forgotten: its row shows `STOPPED` like any other missing agent, so it
can be restarted where it left off. (Revised 2026-11-15 after trying it: the
first version forgot a deliberately closed agent, and its row went quiet.)

## Testing

- Unit: `continue_args` per kind. Detecting "missing" from (session, foreground
  command) pairs, including the "agent exited, shell left behind" case.
- Unit: the restart command line includes the continue argument only for
  supported kinds.
- Manual (dev app): kill a member's tmux session, check the row shows `STOPPED`,
  click it, and the agent comes back with its conversation. Same for the
  coordinator, and check that it re-checks its delegations.

## Left to the builder

- The exact pill styling, following the existing `agent_pill` palette.
- Where the "missing" check is cached, and how often it refreshes, as long as
  it stays off the UI thread.
- The OpenCode and Codex continue-scope checks above.
