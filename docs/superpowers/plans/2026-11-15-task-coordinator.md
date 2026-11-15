# Task Coordinator Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give every multi-project task a dedicated coordinator agent (in the task folder) that the task tile opens, push specialist results into its chat, show A2A traffic as animated connector lines in the sidebar, and remove the TASK AGENTS strip.

**Architecture:** The coordinator is a normal terminal agent whose cwd is `~/.openorchestra/tasks/<slug>/`, with its own pane key and A2A tenant `@coordinator`. The broker treats `@coordinator` as the default target and pushes a reply inbox message to it when a delegation finishes. The app resolves `Selection::Task { member_id: None }` to the coordinator for 2+ member tasks, and the sidebar renders connector state from broker snapshots.

**Tech Stack:** Rust, gpui (zed fork), tmux, the in-repo crates `a2a-broker`, `worktrees`, `agent-runtime`, `harness-app`.

**Spec:** `docs/superpowers/specs/2026-11-15-task-coordinator-design.md`

## Global Constraints

- Coordinator tenant id is exactly `@coordinator`. Worktree ids always contain `/`.
- Coordinator cwd is `tasks_root.join(slug)`, where `tasks_root = data_dir.join("tasks")`.
- Single-member tasks behave exactly as today: no coordinator, and first open is a shell.
- The coordinator never edits repo trees. Every repo member, including the former home, is a specialist.
- Connectors: travel animation 1s; result flash 1.5s; colors are the theme's `hx.amber` (active), `hx.green` (completed), `hx.red` (failed), and `hx.border` (idle).
- Repaint for connector animation only while a pulse or one-shot event is live.
- `crates/harness-app` is its own Cargo workspace. Run `cargo test` there **and** at the repo root, because `a2a-broker` and `worktrees` are shared.
- Never touch the user's running `openOrchestra Dev` app. Do visual checks in a scratch `.app` with its own bundle id and `HOME`.

## Review Focus

1. A delegation finishes while the coordinator has no Agent tab open. The result must queue in `a2a_pending_inject` and appear once the tab opens, exactly once. (Test in Task 4.)
2. `complete.sh` is run twice for the same task id. The second run must not push a second result line. (Test in Task 2.)
3. An existing 2-repo task whose saved selection is `member_id: None` is restored after upgrade. It must land on the coordinator, and the old home repo's tabs must stay on that repo's row. (Test in Task 4.)
4. The coordinator folder isn't a git repo. Agent spawn must not fail or spam warnings from `exclude_locally`, and the status pill must still update. (Test in Task 4.)
5. A delegation is addressed to `@coordinator` or to an unknown tenant. The first is allowed (specialist → coordinator message); the second is rejected with `unknown tenant`. (Test in Task 1.)

---

### Task 1: Broker coordinator identity

**Files:**
- Modify: `crates/a2a-broker/src/lib.rs` (`BrokerConfig`, `MemberEndpoint`, pane-key helpers)
- Modify: `crates/a2a-broker/src/http.rs` (root card handler, `handle_send_message`, test helpers `test_state*`)
- Modify: `crates/a2a-broker/src/card.rs` (uses of `is_home` / home fields)
- Test: `crates/a2a-broker/src/http.rs` `mod tests`, `crates/a2a-broker/src/lib.rs` `mod tests`

**Interfaces:**
- Produces:
  - `pub const COORDINATOR_TENANT: &str = "@coordinator";`
  - `pub fn coordinator_pane_key(task_id: &str) -> String` (= `task_member_pane_key(task_id, COORDINATOR_TENANT)`)
  - `BrokerConfig.home_worktree_id` is renamed to `coordinator_tenant: String`. `home_repo` is removed (fix uses).
  - `MemberEndpoint.is_home` is renamed to `is_coordinator: bool`. The member list includes one coordinator endpoint (`worktree_id: "@coordinator"`, `repo: "coordinator"`, `path: <task dir>`) plus every repo as a specialist.

- [ ] **Step 1: Write the failing tests**
  - `coordinator_pane_key_round_trips`: `parse_task_member_pane_key(&coordinator_pane_key("auth")) == Some(("auth", "@coordinator"))`.
  - `send_without_tenant_targets_the_coordinator`: POST `/jsonrpc` `message/send` (no `/t/`). The created broker task's `tenant == "@coordinator"`.
  - `former_home_repo_is_a_delegation_target`: with members `@coordinator`, `jam/auth`, `api/auth`, sending to `/t/jam%2Fauth/jsonrpc` succeeds and enqueues to `jam/auth`.
  - `root_card_lists_every_repo_as_specialist`: GET `/.well-known/agent-card.json`. The card's sibling list holds `jam/auth` and `api/auth` and not `@coordinator`.
  - `unknown_tenant_is_rejected`: sending to `/t/nope%2Fx/jsonrpc` returns an error containing `unknown tenant`.
  - Update `test_state*` helpers to the new member shape.
- [ ] **Step 2: Run** `cargo test -p a2a-broker`. Expected: the new tests FAIL (or don't compile).
- [ ] **Step 3: Implement** the renames and constants. The root card picks `is_coordinator` as the orchestrator and lists `!is_coordinator` members as siblings. `handle_send_message` defaults to `state.config.coordinator_tenant`.
- [ ] **Step 4: Run** `cargo test -p a2a-broker`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(a2a): coordinator tenant replaces the home member as orchestrator`

### Task 2: Push results to the coordinator

**Files:**
- Modify: `crates/a2a-broker/src/bridge.rs` (`InboxMessage`)
- Modify: `crates/a2a-broker/src/http.rs` (`drain_bridge`)
- Test: `crates/a2a-broker/src/http.rs` `mod tests`

**Interfaces:**
- Consumes: `COORDINATOR_TENANT`, `BrokerState::notify_inbox`.
- Produces:
  - `InboxMessage` gains `#[serde(default)] pub reply: bool`. A reply has `from_tenant: Some(<specialist tenant>)` and `reply: true`. Its `text` is `"<state> by <repo>: <summary-or-reason>"`, where `<state>` is `completed`, `failed` or `rejected` and `<repo>` is the specialist's `MemberEndpoint.repo`.
  - `BridgeEvent::Rejected { a2a_task_id: String, reason: String }` (serialized in the outbox like `Failed`, with `"state":"rejected"`).
  - `/v1/complete` accepts `rejected=1&reason=<text>` (form) or `{"rejected":true,"reason":…}` (JSON), which produces `BridgeEvent::Rejected`. `reason` defaults to `"rejected"`.

- [ ] **Step 1: Write the failing tests**
  - `completion_pushes_one_reply_to_coordinator`: send to `api/auth`, then POST `/v1/complete` with `summary=shipped`. After the drain (poll up to 2s), exactly one `InboxDelivery` arrived on `on_inbox` with `tenant == "@coordinator"`, `message.reply == true`, `message.text == "completed by api: shipped"`, and `message.a2a_task_id` equal to the delegated id. The coordinator inbox file has one line.
  - `failure_pushes_failed_reply`: `/v1/complete` with `failed=1&error=boom` gives the text `"failed by api: boom"`.
  - `rejection_pushes_rejected_reply`: `/v1/complete` with `rejected=1&reason=not+my+repo` sets the broker task state to `rejected`, and gives the text `"rejected by api: not my repo"`.
  - `repeated_complete_does_not_push_twice`: complete the same id twice. Exactly one coordinator delivery.
  - `delegation_inbox_is_not_a_reply`: the specialist delivery from `message/send` has `reply == false`.
- [ ] **Step 2: Run** `cargo test -p a2a-broker`. Expected: FAIL.
- [ ] **Step 3: Implement** in `drain_bridge`: read the task's current state and tenant before `set_state`. Handle `Rejected` like `Failed`, setting `TaskState::Rejected`. Push only when the previous state wasn't terminal (`Completed | Failed | Canceled | Rejected`). Enqueue through `state.bridge.enqueue(COORDINATOR_TENANT, &msg)`, then `state.notify_inbox(...)`. Release the `tasks` lock before notifying. Canceled doesn't push: it only comes from the coordinator's own action.
- [ ] **Step 4: Run** `cargo test -p a2a-broker`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(a2a): push finished delegations to the coordinator`

### Task 3: Coordinator role in the agent context

**Files:**
- Modify: `crates/worktrees/src/tasks.rs` (`task_agent_context_with_a2a`, new helper)
- Test: `crates/worktrees/src/tasks.rs` `mod tests` (update the asserts that use `"Home / orchestrator"`)

**Interfaces:**
- Produces: `pub fn task_coordinator_dir(tasks_root: &Path, slug: &str) -> PathBuf` (= `tasks_root.join(slug)`).
- Context text for a 2+ member task:
  - The header line is `Coordinator: <coordinator dir>`.
  - A `Repos (each has its own specialist agent):` list of every member, as `- <repo> `<path>` (worktree id `<id>`)`.
  - Role section: **"If your cwd is `<coordinator dir>` you are the COORDINATOR."** Plan across all repos, delegate each repo's part with `delegate.sh <worktree_id> "…"`, don't edit any repo tree, and report back to the user. Results arrive in your chat on their own as `[openOrchestra A2A] task_id=… completed by <repo>: …`; use `status.sh` only to check work still in progress.
  - **"Otherwise you are the SPECIALIST"** text as today, plus: if a request is outside your repo or you can't do it, run `{dir}/reject.sh <a2a_task_id> "why"` instead of `complete.sh`.
- Single-member tasks: no role section, unchanged.

- [ ] **Step 1: Write/adjust failing tests**
  - `multi_member_context_names_the_coordinator`: it contains `Coordinator: /oo/tasks/auth` (with `tasks_root=/oo/tasks`, slug `auth`), contains `you are the COORDINATOR`, and does not contain `Home / orchestrator` or `ORCHESTRATOR`.
  - `specialist_text_mentions_reject`: it contains `reject.sh <a2a_task_id>`.
  - `every_repo_is_listed_as_a_specialist`: both member repos appear under `Repos`.
  - `coordinator_dir_is_the_task_folder`: `task_coordinator_dir(Path::new("/oo/tasks"), "auth") == PathBuf::from("/oo/tasks/auth")`.
  - Replace the existing `contains("Home / orchestrator: jam")` asserts.
- [ ] **Step 2: Run** `cargo test -p worktrees`. Expected: FAIL.
- [ ] **Step 3: Implement.** `task_agent_context_with_a2a` needs the coordinator dir. Derive it from `A2aContextInfo.a2a_dir.parent()` (the a2a dir is `<task dir>/a2a`), so no signature change is needed.
- [ ] **Step 4: Run** `cargo test -p worktrees`. Expected: PASS.
- [ ] **Step 5: Commit** `feat(tasks): agent context names a coordinator, every repo a specialist`

### Task 4: App wiring for the coordinator

**Files:**
- Create: `crates/harness-app/src/workspace/coordinator.rs` (pure helpers + tests; add `mod coordinator;` in `workspace.rs`)
- Modify: `crates/harness-app/src/workspace/state.rs` (`resolve_task`, `pane_env`, `ensure_pane`, `agent_command_for`)
- Modify: `crates/harness-app/src/workspace/a2a.rs` (`ensure_task_broker`, `start_broker_with_port_fallback`, `format_a2a_inject`)
- Modify: `crates/harness-app/src/workspace/state/alerts.rs` (`session_prefix_for_path`, `alert_target`)
- Modify: `crates/harness-app/src/workspace/lifecycle.rs` (`start_end`)
- Modify: `crates/harness-app/src/workspace/construct.rs` + `actions.rs` (status watch of coordinator dirs)

**Interfaces:**
- Consumes: `COORDINATOR_TENANT`, `coordinator_pane_key` (Task 1); `InboxMessage.reply` (Task 2); `task_coordinator_dir` (Task 3).
- Produces in `coordinator.rs`:
  - `pub(crate) enum TaskFocus<'a> { Coordinator, Member(&'a worktrees::TaskMember) }`
  - `pub(crate) fn task_focus<'a>(task: &'a worktrees::Task, member_id: Option<&str>) -> Option<TaskFocus<'a>>`: `Coordinator` when `member_id` is `None` and the task has ≥2 members; the matching member when `member_id` is given; the home member when the task has 1 member; `None` if the member isn't found.
  - `pub(crate) fn has_coordinator(task: &worktrees::Task) -> bool` (`members.len() >= 2`).
- `Resolved` for the coordinator: `key = coordinator_pane_key(task_id)`, `path = task_coordinator_dir(...)`, `editor_path`/`compare_ref`/`branch` from the home member, `agent_kind = Some(task.agent_kind)`, `task_id = Some(..)`.

- [ ] **Step 1: Write the failing tests** in `coordinator.rs`
  - `two_member_task_with_no_member_focuses_coordinator`
  - `naming_the_former_home_focuses_that_member` (`member_id = Some(home wt)` → `Member(home)`)
  - `single_member_task_focuses_its_member`
  - `unknown_member_is_none`
  - In `a2a.rs` tests: `reply_inject_has_no_complete_hint`: `format_a2a_inject` for `reply: true` gives `"[openOrchestra A2A] task_id=<id> — completed by api: shipped"` with no `complete.sh`. A non-reply message keeps today's format.
- [ ] **Step 2: Run** `cd crates/harness-app && cargo test coordinator a2a`. Expected: FAIL.
- [ ] **Step 3: Implement**
  - `resolve_task` uses `task_focus`. `create_dir_all` the coordinator dir before first use (in `ensure_pane`, not in `resolve`).
  - `pane_env`: for the coordinator, `HARNESS_A2A_TENANT=@coordinator`, the task context path, `HARNESS_TASK_ID`/slug, and the home member's `HARNESS_HOME_WORKTREE`. Omit `HARNESS_WORKTREE`/`HARNESS_REPO_ROOT`/branch.
  - `ensure_pane`: open `TabKind::Agent(task.agent_kind)` on first open when the focus is `Coordinator`; members open `TabKind::Shell`. This replaces the home-agent branch.
  - `agent_command_for`: skip `exclude_locally` when `r.path` has no `.git` (Review Focus 4). After `on_spawn`, `status_source.watch(&r.path, kind)` so the coordinator's status file drives a pill.
  - `write_a2a_helpers`: also write an executable `reject.sh` (`TASK_ID="${1:?a2a_task_id}"`, `REASON="${2:-rejected}"`; POST form `a2a_task_id`, `rejected=1`, `reason` to `/v1/complete` with the bearer key, mirroring `complete.sh`). Add `reject` to `mcp-tools.json`. Test: `helpers_include_reject_script` checks the file exists and is executable, and contains `rejected=1`.
  - `ensure_task_broker`: build members as one coordinator endpoint plus all repos with `is_coordinator: false`. Set `coordinator_tenant: COORDINATOR_TENANT`. Drop the `home` parameter from `start_broker_with_port_fallback`.
  - `deliver_a2a_inbox` needs no change: tenant `@coordinator` maps to `coordinator_pane_key` through `task_member_pane_key`.
  - `session_prefix_for_path`: a path equal to an active task's coordinator dir maps to `session_prefix(&coordinator_pane_key(id))`. `alert_target` labels it `("<task name> · coordinator", "", <task id>)`, so the notification click navigates to the task.
  - `start_end`: also tear down `coordinator_pane_key(tid)` sessions and the pane entry.
  - Startup (`construct.rs`) and refresh (`actions.rs`): `watch` each active 2+ member task's coordinator dir with `task.agent_kind`.
  - `panes_state.rs` legacy migration (old `task:{id}` → home key) stays as is (Review Focus 3).
- [ ] **Step 4: Add the Review Focus tests**
  - `restored_none_selection_is_coordinator`: this is `two_member_task_with_no_member_focuses_coordinator` again; name it for the restore case.
  - In `a2a.rs`: `reply_queued_without_agent_tab_flushes_once`. Drive `a2a_pending_inject` through its pure pieces if `HarnessWorkspace` can't be built in tests. Otherwise move this check to the Task 6 manual check and say so in the commit.
- [ ] **Step 5: Run** `cd crates/harness-app && cargo test` and `cargo test` at the repo root. Expected: PASS.
- [ ] **Step 6: Commit** `feat(app): multi-project tasks open a dedicated coordinator agent`

### Task 5: Sidebar, top bar and dialog, plus connector lines

**Files:**
- Create: `crates/harness-app/src/workspace/ui/sidebar/links.rs` (pure link state + tests; `mod links;` in `sidebar.rs`)
- Modify: `crates/harness-app/src/workspace/ui/sidebar.rs` (`render_task_row`, members block)
- Modify: `crates/harness-app/src/workspace/ui/chrome.rs` (delete `render_task_strip`; coordinator breadcrumb)
- Modify: `crates/harness-app/src/workspace/render.rs:72` (drop the strip call)
- Modify: `crates/harness-app/src/workspace/ui/new_task.rs` (hide the Home radio for ≥2 repos; info text)
- Modify: `crates/harness-app/src/workspace/construct.rs` (a2a change handler records link events)
- Modify: `crates/harness-app/src/workspace.rs` (new field `a2a_links: links::LinkTracker`)

**Interfaces:**
- Consumes: `a2a_delegation_summary()` rows `(a2a_task_id, tenant, state)`; `anim_secs()`; `has_coordinator`, `TaskFocus` (Task 4).
- Produces in `links.rs`:
  - `pub(crate) enum LinkEventKind { Down, Up { ok: bool } }`
  - `pub(crate) struct LinkEvent { pub task_id: String, pub tenant: String, pub kind: LinkEventKind, pub started: f32 }`
  - `pub(crate) fn diff_snapshots(task_id: &str, prev: &[(String, String, String)], next: &[(String, String, String)], now: f32) -> Vec<LinkEvent>`: a new id gives `Down` for its tenant; an id whose state became `completed` gives `Up { ok: true }`; one that became `failed`/`rejected` gives `Up { ok: false }`; ids with no change give nothing.
  - `pub(crate) enum LinkVisual { Idle, Active { pulse: f32 }, Travel { kind: LinkEventKind, progress: f32 }, Flash { ok: bool, fade: f32 } }`
  - `pub(crate) fn link_visual(active: bool, events: &[&LinkEvent], now: f32) -> LinkVisual`: the newest live event wins. `Travel` lasts 0–1.0s. After an `Up`, `Flash` lasts 1.0–2.5s. Otherwise it is `Active` if `active` (pulse = `0.5 + 0.5*sin(now*2π/1.6)`), else `Idle`.
  - `pub(crate) struct LinkTracker { prev: HashMap<String, Vec<(String,String,String)>>, events: Vec<LinkEvent> }` with `fn observe(&mut self, task_id, rows, now)`, `fn events_for(&self, task_id, tenant) -> Vec<&LinkEvent>`, `fn is_animating(&self, now) -> bool`, and `fn prune(&mut self, now)` (drop events older than 2.5s).

- [ ] **Step 1: Write the failing tests** in `links.rs`
  - `new_delegation_is_a_down_event`
  - `completion_is_an_up_ok_event`; `failure_is_an_up_not_ok_event`
  - `unchanged_rows_emit_nothing`
  - `travel_then_flash_then_idle`: `link_visual(false, [Up ok @0], t)` gives `Travel` at t=0.5, `Flash{ok:true}` at t=1.5, and `Idle` at t=3.0.
  - `active_without_events_pulses`
  - `tracker_stops_animating_after_events_expire` (`is_animating(now)` is false at 2.6s with no active link)
- [ ] **Step 2: Run** `cd crates/harness-app && cargo test links`. Expected: FAIL.
- [ ] **Step 3: Implement `links.rs`.**
- [ ] **Step 4: Run** the same tests. Expected: PASS.
- [ ] **Step 5: Wire the UI**
  - Delete `render_task_strip` and its call.
  - In the a2a change handler, call `ws.a2a_links.observe(task_id, rows, ws.anim_secs())` for every task with a running broker (snapshot each broker's `list_for_context`, not just the selected one), then `cx.notify()`.
  - `render_task_row` (≥2 members):
    - The tile shows the coordinator's `agent_pill`, from `agent_status[coordinator dir]`.
    - It shows an amber `N A2A` before the pill when N > 0 (working + submitted across the task).
    - Tooltip: `Coordinator: plans the task and delegates to each repo's agent`.
    - It is highlighted when the focus is `Coordinator`.
  - Repo rows: no `★`, no accent color. Highlight only when `member_id == Some(m.worktree_id)`.
  - Replace the members block's `border_l_1` with absolutely positioned connectors in the same 12px gutter: a spine plus a per-row elbow. Color and opacity come from `link_visual`. `Travel` draws a 5px dot at `progress` along spine+elbow (down = tile→row, up = row→tile). Single-member tasks keep today's plain rule.
  - While `a2a_links.is_animating(now)` or any link is active, request the next frame the way the waiting pill's animation does (reuse that mechanism; don't add a new timer). `prune` on each observe.
  - Top bar (`chrome.rs`): with the coordinator focused, the breadcrumb is `<task name> · coordinator` in place of repo/branch. Keep `render_a2a_delegation_chip`.
  - `new_task.rs`: with ≥2 repos checked, hide the Home radios and still write `new_task_home` = the first checked repo. Info text: `"Name labels the effort. Brief is the starting prompt injected into agents. Multi-project tasks get a coordinator agent that delegates to each project's agent over A2A — project AGENTS.md is never overwritten."`
- [ ] **Step 6: Run** `cd crates/harness-app && cargo build && cargo test && cargo clippy`. Expected: no new warnings beyond the known linker note.
- [ ] **Step 7: Commit** `feat(ui): coordinator on the task tile, live A2A connector lines, drop TASK AGENTS strip`

### Task 6: End-to-end check

- [ ] **Step 1:** `cargo test` at the repo root and in `crates/harness-app`. Expected: all green.
- [ ] **Step 2:** Build a scratch bundle following the memory note: copy `target/debug/harness-app` into `<scratch>/oo-coord.app` with bundle id `ai.openorchestra.coordtest`, ad-hoc sign it, and run `open -n --env HOME=<scratch>/home <bundle>`. Create a 2-repo task with two throwaway git repos.
- [ ] **Step 3:** Verify:
  - There is no TASK AGENTS strip.
  - The tile opens the coordinator agent, with cwd = the task folder.
  - Each repo row opens its own agent.
  - Run `delegate.sh <wt> "say hi"` from the coordinator. A dot travels down and the line turns amber.
  - Run `complete.sh <id> "done"` from the specialist. A dot travels up, the line flashes green, and exactly one `completed by <repo>: done` line appears in the coordinator.
  - Close the coordinator tab, complete another delegation, and reopen it. The line appears once.
- [ ] **Step 4:** Report the results with screenshots. Fix forward in the owning task if anything fails.
