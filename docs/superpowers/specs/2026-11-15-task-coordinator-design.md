# Task coordinator: a dedicated A2A agent per multi-project task

Status: design, approved approach (2026-11-15). Supersedes the "home member is
the orchestrator" role split in
[2026-08-09-multi-context-a2a-orchestration-design.md](2026-08-09-multi-context-a2a-orchestration-design.md).

## Intent

In a multi-project task it isn't clear which agent coordinates the work. Today
the HOME repo's agent (★) does two jobs: it is the chat for the whole task *and*
the coder for its own repo. On top of that, the TASK AGENTS strip under the top
bar repeats what the sidebar's task card already shows.

The user's model is: one agent per repo, plus one more agent that runs the A2A
coordination. This design makes that true and makes it obvious in the UI.

## Goals

- A multi-project task (2+ repos) has a **coordinator** agent that is not any
  repo's agent. With N repos there are N specialists + 1 coordinator.
- Clicking the task tile in the sidebar opens the coordinator. Clicking a repo
  row opens that repo's specialist.
- The TASK AGENTS strip is removed.
- Every repo is a plain specialist. No repo is marked special (no ★ in the
  sidebar, no "orchestrator" label on a repo).

## Non-goals

- Single-repo tasks don't change. They get no coordinator, and clicking them
  opens a shell as it does today.
- No push of specialist completions into the coordinator's session. The
  coordinator still checks progress with `status.sh`, as HOME does today. This
  is a good follow-up but a separate change.
- No new A2A protocol features.

## Anti-goals

- The coordinator must not be able to pass for a repo agent. If it runs inside a
  repo checkout, or shares a pane key or status entry with one, the design has
  failed even if it works.
- Don't add a new surface (strip, panel, extra bar) that repeats the sidebar.

## Constraints found in the code

- Agent status, hook state and the waiting pill are keyed by **working
  directory** (`agent_status: path → AgentState`; Claude Code hooks are written
  to `<cwd>/.claude/settings.local.json`). Two agents can't share a cwd.
- Pane sets are keyed by `a2a_broker::task_member_pane_key(task_id, worktree_id)`
  (`task:{id}:member:{wt}`). A2A inbox delivery injects into the pane for a
  tenant via that key.
- The broker's notion of "home" (`BrokerConfig.home_worktree_id`,
  `MemberEndpoint.is_home`, the default target in `handle_send_message`, and the
  home card that lists siblings) is the orchestrator identity.
- `Task.home_repo` is persisted in the task store and is also used for the task
  branch line, the default diff and editor focus, and the New Task dialog's
  Home radio.

## Approaches considered

1. **Coordinator in the task's own folder** (`~/.openorchestra/tasks/<slug>/`,
   which already holds `brief.md`, `agent-context.md` and `a2a/`). It is a normal
   terminal agent of the task's agent kind, with its own cwd, pane set and A2A
   tenant. It can read every repo and edits none. **Picked.** It is the only
   option that gives the coordinator its own status and hooks without touching a
   repo, and it keeps the user's choice of agent.
2. Coordinator as a second agent inside the HOME worktree. Less wiring, but it
   collides with the repo agent on cwd-keyed status and hooks, and nothing stops
   it from editing that repo. Rejected: it is the mixing we are removing.
3. Coordinator as the built-in pi RPC chat panel. Native approval UI, but it locks
   the coordinator to pi while every other agent is user-chosen. Rejected.

## Design

### Identity

- Coordinator tenant id: `@coordinator`. A worktree id always contains `/`, so
  this can't collide with one.
- Coordinator pane key: `task_member_pane_key(task_id, "@coordinator")`. Add a
  named constant/helper in `a2a-broker` rather than spelling the string out at
  each call site.
- Coordinator cwd: `tasks_root/<slug>/` (create it if missing). Agent kind:
  `task.agent_kind`.

### Selection and resolve

- `Selection::Task { member_id: None }` for a task with 2+ members resolves to
  the coordinator: `path` = task folder (agent and shell cwd), key = coordinator
  pane key.
  - `editor_path`, `compare_ref` and `branch` come from the home member (the
    first member if `home_repo` is unset), so the Editor and DIFF still show a
    real checkout.
- `member_id: Some(wt)` resolves to that specialist exactly as today.
- With 1 member, `member_id: None` keeps resolving to that member (unchanged).
- `ensure_pane`: on first open of the coordinator, open an Agent tab (as the
  HOME branch does today). Specialists open a shell on first open, as
  non-home members do today.
- `pane_env` for the coordinator sets `HARNESS_A2A_TENANT=@coordinator` and the
  task context path, plus the `HARNESS_*` vars that make sense without a repo.
  Repo-specific vars (`HARNESS_WORKTREE`, branch, repo root) point at the home
  member, or are omitted. This is the builder's call; document which one.

### A2A broker

- The broker gains a coordinator endpoint (tenant `@coordinator`, path = task
  folder) that takes the role `is_home` plays today: the default target, and the
  card that lists the specialists.
- **Every** repo member, including the former home, is listed as a specialist
  and can receive delegations.
- `BrokerConfig.home_worktree_id` becomes the coordinator tenant for
  multi-member tasks. Rename it where that's cheap; otherwise document the new
  meaning.
- Inbox delivery to `@coordinator` injects into the coordinator pane via the
  same `deliver_a2a_inbox` path.

### Agent context (`worktrees::task_agent_context_with_a2a`)

- "Your role" is rewritten by cwd:
  - **cwd = the task folder: you are the COORDINATOR.** Plan across all repos,
    and delegate each repo's part with `delegate.sh <worktree_id> "…"` and
    `status.sh`. Don't edit any repo tree. Report back to the user.
  - **Otherwise you are the SPECIALIST** for your cwd's repo (text as today).
- The "Home / orchestrator: …" header line and "Specialists (do not edit these
  trees from home …)" list become "Coordinator: <task folder>" and a list of all
  repos.
- Update the tests in `crates/worktrees/src/tasks.rs` that assert on
  "Home / orchestrator".

### UI

- **Remove** `render_task_strip` (`ui/chrome.rs`) and its call in `render.rs`.
- **Sidebar task tile** (`render_task_row`), for 2+ members:
  - It shows the coordinator's agent state pill, and an amber `N A2A` count
    when delegations are working or submitted. Show the count only when it is
    above zero.
  - It is highlighted when the coordinator is selected. Its tooltip says
    "Coordinator: plans the task and delegates to each repo's agent".
- **Repo rows:** drop the `★` prefix and the accent color for home. A repo row
  is highlighted only when *that* specialist is selected. Remove today's rule
  that lights up the home row when `member_id` is `None`.
- **Top bar:** when the coordinator is selected, the breadcrumb reads
  `<task name> · coordinator` instead of a repo/branch. Keep the existing
  `render_a2a_delegation_chip`.
- **New Task dialog:** with 2+ projects selected, hide the Home radio and
  change the info text to say a coordinator agent is created for the task.
  `home_repo` is still written (the first selected repo) so that the default
  editor and diff focus and older code keep working.

### Lifecycle and migration

- Existing multi-project tasks get a coordinator the first time they are
  selected. No store migration: the coordinator is derived from the task.
- The old HOME repo's saved pane set (`task:{id}:member:{home_wt}`) stays the
  home repo's specialist. Agents already running keep the old role text until
  they restart. That is acceptable: the context file is rewritten, and new
  sessions read it.
- Archive and delete of a task also kill the coordinator's tmux session and
  remove its pane entry. The task folder is already handled by the existing
  task cleanup; verify this.
- Agent hooks written into the task folder (e.g.
  `tasks/<slug>/.claude/settings.local.json`) are fine because the folder is not
  a git repo. Any adapter step that assumes a git repo (git excludes,
  `ignored_paths`) must be skipped or tolerate a non-repo cwd.

## Builder's calls (marked as my calls, not the user's)

- The tenant string `@coordinator` and the pane-key helper name.
- Keeping the top-bar delegation chip while adding the count to the tile.
- Hiding, not deleting, the Home field for multi-project tasks, and keeping
  `home_repo` in the store.
- Which repo-specific `HARNESS_*` vars the coordinator gets.

## Verification

- Unit tests: resolve (coordinator vs specialist vs single-member), the
  agent-context role text, the broker treating the former home as a specialist
  and `@coordinator` as the default target, and pane-key helpers.
- `cargo test` in **both** `crates/harness-app` and the repo root (shared crates
  `worktrees` and `a2a-broker` change).
- Visual check in an isolated scratch app bundle: the strip is gone; the
  2-repo task tile opens the coordinator; each repo row opens its own agent; a
  delegation from the coordinator lands in the target repo's agent, and the
  tile count updates.
