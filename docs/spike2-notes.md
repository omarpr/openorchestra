# Spike 2 notes — status + forge pipeline

**Status:** Done · July 19, 2026 · proves the "mission control" layer (architecture §9 Spike 2)
**Pieces:** `pi-ext/harness-status.ts` → `.harness/status.json` → `crates/status-watch` → demo; `crates/forge` (gh poller behind a trait).

---

## 1. Real pi extension API (v0.70.6) vs the architecture sketch

First finding: the npm package **`@mariozechner/pi` is NOT the coding agent** — at
0.70.6 it is the unrelated "pods" (vLLM deployment) CLI from the same monorepo.
The coding agent (the `pi` binary, the extension API, the docs) is
**`@mariozechner/pi-coding-agent`** (`bin: pi`), same repo
`badlogic/pi-mono`, directory `packages/coding-agent`. Pin that package.

**Registration surface:** an extension is a TS module whose default export is a
factory `(pi: ExtensionAPI) => void | Promise<void>`; it subscribes with
`pi.on(eventName, handler)`. Handlers get `(event, ctx: ExtensionContext)`;
`ctx` exposes `cwd`, `ui`, `sessionManager` (→ `getSessionFile()`), `isIdle()`,
`hasPendingMessages()`, `abort()`, etc. Extensions are loaded via jiti (no
compile step) from:

- `~/.pi/agent/extensions/*.ts` (global), `.pi/extensions/*.ts` (project-local),
- `pi -e <path>` / `--extension`,
- `settings.json` `"extensions": [paths]` or installable pi packages.

For the harness: symlink/copy `pi-ext/harness-status.ts` into each worktree's
`.pi/extensions/` (or reference it from settings) when spawning the pty.

**Full extension event union (v0.70.6):** `resources_discover`, `session_start`,
`session_before_switch`, `session_before_fork`, `session_before_compact`,
`session_compact`, `session_shutdown`, `session_before_tree`, `session_tree`,
`context`, `before_provider_request`, `after_provider_response`,
`before_agent_start`, `agent_start`, `agent_end`, `turn_start`, `turn_end`,
`message_start`, `message_update`, `message_end`, `tool_execution_start`,
`tool_execution_update`, `tool_execution_end`, `model_select`, `tool_call`,
`tool_result`, `user_bash`, `input`.

### Mapping (architecture §4.1 sketch → reality)

| Sketch | Reality at v0.70.6 | State written |
|---|---|---|
| `agent_start` | `agent_start` — exists, 1:1 | `running` |
| "UI dialog open" | **No such event.** All extensions of a session share one `ExtensionUIContext` instance (`ExtensionRunner.uiContext`, returned by every `ctx.ui`). The status extension wraps its `select`/`confirm`/`input`/`editor` methods; any extension opening a dialog (i.e. the approval-gate path, D9) flips the state while the promise is pending. pi-core-owned TUI dialogs (model picker etc.) are NOT visible this way — acceptable: those are user-initiated, not "agent blocked". | `waiting` |
| `agent_settled` | **RPC-mode-only event; does not exist for extensions.** Approximated as `agent_end` + `!ctx.hasPendingMessages()` (queued steer/follow-up ⇒ stay running) + a debounced write (`HARNESS_STATUS_SETTLE_MS`, default 750 ms) cancelled by a subsequent `agent_start`. | `idle` |
| — | `session_start` (reasons: startup/new/resume/fork/reload) | initial `idle` + fresh `sessionFile` |
| — | `session_shutdown` (quit/reload/new/resume/fork) | forced `idle` |

**Empirically observed** (real pi 0.70.6, headless `-p` run, provider erroring):
`session_start → (agent_start → turn_start → turn_end → agent_end) × 4 → session_shutdown`
— i.e. **auto-retry re-fires `agent_end`/`agent_start` per attempt** and the
extension `agent_end` event carries no `willRetry` flag (unlike RPC). The
debounce absorbs quick retries; retry gaps longer than the debounce still show
an idle blip (see R8 below).

## 2. Verification level achieved for the extension

Verified against **real pi v0.70.6** in a throwaway dir (headless `-p` print
mode; no model available in this container — the sandbox blocks even localhost
sockets, so every provider call errs with "Connection error", which is itself a
usable lifecycle exercise):

- extension loads cleanly via `pi -e` (jiti, no compile step), no startup errors;
- `.harness/status.json` is created and atomically updated; final content
  `{"state":"idle", ..., "sessionFile":"<real session .jsonl path>"}`;
  `--no-session` runs correctly produce `"sessionFile": null`;
- lifecycle event order captured with a side-by-side logging extension (above);
- **end-to-end demo ran**: `status-watch` demo binary watching the dir printed
  `unknown → idle → running → idle → …` live while pi ran.

Verified by **mock harness** (Node, extension loaded through jiti with a faked
`ExtensionAPI`/`ExtensionContext`): full state machine — session_start→idle,
agent_start→running, shared-ui dialog→waiting→back to running, retry
(agent_end then agent_start inside debounce) does NOT flap to idle,
settle→idle, queued messages keep running, shutdown→idle, no temp files left.

**Not verified** (no model, no TTY): a real streamed completion; a real
approval dialog raised by pi's interactive TUI; behavior under `/reload`,
`/fork`, `/compact`. None are schema-affecting.

## 3. `status.json` schema — FROZEN

Path: `<worktree-root>/.harness/status.json` (add `.harness/` to gitignore).
Written atomically: temp file in `.harness/` + `rename(2)`. Watch the
**directory**, not the file inode.

```json
{
  "state": "running" | "waiting" | "idle",
  "timestamp": "2026-07-19T09:40:48.691Z",
  "sessionFile": "/abs/path/to/session.jsonl" | null
}
```

- `state`: exactly these three strings. Consumers MUST treat unrecognized
  strings as "unknown" (forward compatibility — Rust side maps them to
  `AgentState::Unknown`), a missing file as unknown/idle, and malformed JSON
  as "keep last good".
- `timestamp`: ISO-8601 UTC with milliseconds (`Date.toISOString()`).
- `sessionFile`: absolute path to pi's session JSONL, `null` for ephemeral
  sessions. This is the training-data harvesting hook (§6.4).
- Future additive fields (e.g. outcome metadata) are allowed; consumers must
  ignore unknown fields (serde default behavior — already true).

Spike 1's watcher and Phase 2 both key off this file; changing it is a
schema migration.

## 4. crates/status-watch

`StatusWatcher::new() -> (StatusWatcher, crossbeam Receiver<StatusEvent>)`;
`watch(root)` / `unwatch(root)` / `watched()`. `watch` creates `.harness/` if
absent (dir must exist to be watched; extension also creates it — first one
wins), watches it non-recursively via `notify` v8 (inotify/FSEvents), and
immediately delivers the current status (`Unknown` if no file). Malformed JSON
is skipped (last good kept, nothing delivered); duplicate content is deduped
even when the OS reports several events per rename.

Tests (9, real tempdir files, atomic-rename writes): initial missing/present,
running→waiting→idle transitions, malformed-JSON tolerance + recovery,
unknown-state forward compat, null sessionFile, multi-root keying, unwatch,
duplicate suppression.

## 5. crates/forge

`Forge` trait (§6.3): `available()`,
`pr_for_branch(repo_dir, branch) -> Result<Option<PrInfo>, ForgeError>`,
`checks_for_branch(repo_dir, branch) -> Result<Vec<CheckRun>, ForgeError>`.
(Deviation from the sketch: methods return `Result<Option<…>>` rather than bare
`Option` so the poller can distinguish "no PR" from "gh failed" for backoff.)
`PrInfo { number, state: Open|Draft|Merged|Closed, title }`,
`CheckRun { name, status: Pass|Fail|Running|Skipped }`.

`GhCli` implementation shells out through a `CommandRunner` trait
(`SystemRunner` for real use; tests inject a playback fake — **gh is not
installed in this container, parsing is fixture-tested only**):

- `gh pr view <branch> --json number,state,isDraft,title`
  Fields verified against the gh manual. `state` ∈ `OPEN|CLOSED|MERGED`
  (GraphQL PullRequestState); `OPEN + isDraft:true` → `Draft`. Exit ≠ 0 with
  stderr `no pull requests found for branch "…"` → `Ok(None)`; other failures
  → `CommandFailed`; unparsable stdout → `Parse` (no panics anywhere).
- `gh pr checks <branch> --json name,state,bucket`
  Manual-verified field set for checks: `bucket, completedAt, description,
  event, link, name, startedAt, state, workflow`. `bucket` "categorizes the
  state field into `pass`, `fail`, `pending`, `skipping`, or `cancel`".
  Mapping: pass→Pass, fail/cancel→Fail, pending→Running, skipping→Skipped;
  unknown bucket falls back to raw `state` (SUCCESS/FAILURE/IN_PROGRESS/…).
  **gh uses exit codes as signal here** (8 = checks pending, non-zero on
  failing checks), so stdout is parsed regardless of exit code; stderr
  `no checks reported …` / no-PR → `Ok(vec![])`.
- `available()`: probes `gh --version` at construction; when false every call
  returns `ForgeError::Unavailable` and `Poller` exits immediately — callers
  hide badges (R7).

`Poller::spawn(Arc<dyn Forge>, branches, PollerConfig) -> (Poller, Receiver<ForgeUpdate>)`:
background thread, per-branch schedule of `interval` (default 45 s) + uniform
jitter (default 0–10 s, xorshift — no rand dep), exponential per-branch backoff
on failure (15 s → ×2 → cap 300 s, reset on success). Change-driven delivery:
identical consecutive results and repeated identical errors are suppressed.
`add_branch`/`remove_branch` at runtime; `stop()`/`Drop` joins the thread.
When a branch has no PR, the checks call is skipped (halves gh invocations).

Fixtures (`crates/forge/fixtures/`): open/draft/merged/closed PR, mixed checks
(pass/fail/pending×2/skipped/cancelled), all-pass with exit 8, unknown bucket,
no-PR stderr, no-checks stderr, garbage output. 21 forge tests total
(16 parsing/availability + 5 poller + 2 unit for backoff/jitter).

## 6. R7 / R8 risk updates

- **R7 (gh CLI)** — *mitigated as designed, but parsing is fixture-verified
  only*: gh could not be installed in the build container, so the JSON shapes
  come from the gh manual (cli.github.com/manual), not recorded from a live
  binary. Field *names* are documented API and safe; exact stderr strings
  (`no pull requests found …`, `no checks reported …`) and the exit-8
  convention are from gh's documented/known behavior and should be confirmed
  once against a real gh (~5 min with any GitHub repo) before trusting the
  no-PR path. Missing-binary degradation is fully tested. Backoff + jitter
  implemented.
- **R8 (file-watch coarseness)** — *confirmed, two concrete gaps found*:
  1. auto-retry fires `agent_end`→`agent_start` per attempt with no
     `willRetry` in the extension API; the debounced idle write (750 ms,
     tunable via `HARNESS_STATUS_SETTLE_MS`) absorbs fast retries but longer
     provider-backoff gaps still blip idle. 2. "waiting" only sees dialogs
     opened through the shared extension UI object — pi-core TUI dialogs are
     invisible. Both are exactly the fidelity ceiling §8 predicted; Phase 2's
     RPC `agent_settled` + `extension_ui_request` events remove it. Keep the
     extension evented (it is — zero polling).
- New minor risk: the shared-`ctx.ui` wrap relies on an undocumented
  implementation detail (single `uiContext` object per runner). It's guarded
  (idempotent WeakSet patching, failure-safe), but re-check on every pi
  version bump (R3 discipline).

## 7. How to run

```sh
# All Rust tests (workspace root; crates/harness-app is excluded by design):
cargo test                                # 33 pass: 9 watch, 21 forge, 1 doc, 2 unit

# Demo — watcher output without the UI:
cargo run -p status-watch --example demo -- /path/to/worktree [more...]
#   → [repo/wt-nav] agent: running

# Feed it from real pi (needs @mariozechner/pi-coding-agent installed):
cd /path/to/worktree && pi -e /path/to/omar-harness/pi-ext/harness-status.ts
# or fake a write:
mkdir -p .harness && printf '{"state":"running","timestamp":"2026-07-19T00:00:00Z","sessionFile":null}' > .harness/t && mv .harness/t .harness/status.json

# Extension state-machine mock test (loads harness-status.ts through jiti,
# the same loader real pi uses):
cd pi-ext && npm install && npm test      # 10 assertions
```

## 8. Files

- `pi-ext/harness-status.ts` — the extension (self-documenting header).
- `pi-ext/harness-status.test.mjs` + `pi-ext/package.json` — mock-harness test
  (pins `@mariozechner/pi-coding-agent@0.70.6` as the dev dep — R3 pin).
- `crates/status-watch/{src/lib.rs, tests/watch.rs, examples/demo.rs}`
- `crates/forge/{src/lib.rs, src/gh.rs, src/runner.rs, src/poller.rs, tests/, fixtures/}`
