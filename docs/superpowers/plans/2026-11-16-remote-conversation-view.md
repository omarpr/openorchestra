# Remote Conversation View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a phone follow a marked Claude Code or pi agent as a readable conversation (messages verbatim, one-line tool calls, collapsed thinking, live status bar) next to the existing terminal view.

**Architecture:** oo-remote gains `conversation.rs` (transcript readers → common `Entry` list) and `transcript.rs` (shared per-file tailer that diffs the active branch and broadcasts updates). Two authenticated endpoints stream entries and serve one tool result on demand. The desktop app passes each marked tab's transcript path in the snapshot (never sent to the phone). The PWA adds a Conversation pane with a status bar beside the Terminal pane.

**Tech Stack:** Rust (serde_json, tokio, axum SSE, notify), React 18 + TS + Tailwind + shadcn/ui, a small Markdown renderer that escapes raw HTML, vitest.

**Spec:** `docs/superpowers/specs/2026-11-16-remote-conversation-view-design.md` (read it, incl. "Decisions from the builder check"). Builds on `docs/superpowers/specs/2026-11-16-oo-remote-design.md` and the code on `feat/oo-remote`.

## Global Constraints

- The transcript path comes only from the app's snapshot for a currently marked tab; it never appears in any response or log line sent to a client.
- Every conversation request/stream is authenticated and re-checked against current marks (same rules as `/api/term`); unmark → `closed: unmarked`, revoke → `revoked`, disable → `disabled`.
- Window: last **200** entries. Read at most the last **8 MiB** of a transcript (skip the first partial line). Tool result fetch capped at **64 KiB**; thinking text at **4 KiB**; user/assistant text at **64 KiB** per message — truncation appends `…[truncated]`.
- Messages verbatim — no summarising. Skip sidechain records, `isMeta` records, bookkeeping types, empty text blocks.
- Supported transcripts: Claude Code (`agent_kind` `"claude-code"`) and pi (`"pi"`) — the `AgentKind::as_str` strings in `crates/worktrees/src/registry.rs`; every other kind keeps the Terminal view only.
- No parsing or file I/O on the GPUI main thread beyond the existing snapshot build.
- Live tmux tests use private `-L` sockets; never the default tmux server; never launch the app in tests.
- `crates/harness-app` is outside the root workspace — build/test it from its own directory; don't run crate-wide `cargo fmt` there.
- Copy (exact): status bar `Working…`, `Needs your input`, `Agent stopped`; thinking `💭 Thinking…`, `💭 Thought for {N} s`, `💭 Thinking (not recorded)`; notice `Can't read the conversation right now`; pill `New messages ↓`; tabs `Conversation` / `Terminal`.

## Review Focus

1. **The user rewinds or forks in Claude Code** → the phone shows the new branch (reset), not old + new mixed. → Task 2 `fork_after_send_resets`.
2. **The agent starts a new session in the same tab** (new transcript path) → the view switches to the new conversation. → Task 3 `new_transcript_path_replaces_stream`.
3. **A tool printed megabytes** (build log) → tap shows the first 64 KiB with a truncation marker; the stream itself never carries it. → Task 1 `tool_result_is_capped`, Task 3 `tool_endpoint_caps_output`.
4. **The user scrolled up to read while new entries arrive** → no jump; a `New messages ↓` pill appears. → Task 5 `does_not_jump_when_scrolled_up`.
5. **Transcript path reported before the file exists (pi at startup)** → empty view that fills in when the file appears. → Task 2 `missing_file_then_created`.

---

### Task 0: PWA polish (name, header, agent icons, status pills, pair once on iOS)

User-requested fixes to the existing PWA, done first.

**Files:**
- Modify: `web/remote/vite.config.ts` (manifest), `web/remote/index.html` (`<title>`), `web/remote/src/App.tsx` (header), `web/remote/src/components/AgentCard.tsx`, `web/remote/src/components/StatusPill.tsx`, `web/remote/src/components/PairScreen.tsx` (or `App.tsx` gate)
- Create: `web/remote/src/components/AgentIcon.tsx`, `web/remote/src/components/InstallFirst.tsx`, `web/remote/src/lib/standalone.ts`; copy `crates/harness-app/icons/agents/{claudecode,codex,cursor,kimi,opencode,pi}.svg` (+ `ATTRIBUTION.md`, `LICENSE`) to `web/remote/src/assets/agents/`; header logo from `assets/openOrchestra-1024.png` (resize to 64 px PNG in `web/remote/public/`)
- Test: `web/remote/src/components/AgentCard.test.tsx`, `StatusPill.test.tsx`, `web/remote/src/lib/standalone.test.ts`

**Requirements (exact):**
- Manifest `name` and `short_name`: `openOrchestra`; `<title>openOrchestra</title>`.
- Header (phone and tablet list column): app logo (24 px, rounded) at left, then `openOrchestra Agents`.
- `AgentIcon({kind})`: `claude-code`→claudecode.svg, `codex`, `kimi`, `opencode`, `pi`, `cursor`|`cursor-agent`→cursor.svg, tinted with the app's colours (`pi #8b7cf6`, `claude-code #d97757`, `codex #10a37f`, `kimi #4f9cf9`, `opencode #3fb950`); unknown kind → monogram `◇` in `#9aa3af`. 16 px, shown left of the title on the card's first line.
- AgentCard: remove the separate agent-kind badge/line (the icon replaces it); keep `project · worktree`, status, time, tool.
- StatusPill matches the desktop app: `running` → accent (`#5b9bff` dark / `#1f6feb` light) text on a 16 % tint, spinning arc (1 s per turn, CSS), label `RUNNING`; `waiting` → amber (`#e0a83b` / `#9a6700`) with a dot breathing between 35 % and 100 % opacity over 1.2 s, label `WAITING`; `idle` and `unknown` → render nothing; `stopped` → muted (`#646b76` / `#7b8493`) outline dot, label `STOPPED`. `prefers-reduced-motion` → no spin/breath (static indicator). Agent list sort order unchanged.
- Pair once on iOS: `isStandalone()` = `matchMedia('(display-mode: standalone)').matches || navigator.standalone === true`; `isIOS()` = iPhone/iPad/iPod UA or (Macintosh UA and `maxTouchPoints > 1`). When there is no token and `isIOS() && !isStandalone()`, show `InstallFirst` instead of the pair form: heading `Add openOrchestra to your Home Screen`, steps `Tap Share`, `Choose Add to Home Screen`, `Open openOrchestra from your Home Screen and pair there`, plus a small link `Pair in this browser instead` that reveals the normal pair form. Android/desktop unchanged.

- [ ] **Step 1: Failing tests:** AgentCard renders the claude icon (by `data-agent="claude-code"` or alt text) and does NOT render the text `claude-code`; StatusPill renders nothing for idle/unknown, `RUNNING` with a spinner element for running, `WAITING` for waiting, `STOPPED` for stopped, no animation class under reduced motion; `isStandalone`/`isIOS` truth table; App shows `InstallFirst` for iOS non-standalone without token, the pair form for Android, and the pair form after tapping `Pair in this browser instead`; header text `openOrchestra Agents`.
- [ ] **Step 2:** `pnpm -C web/remote test` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `pnpm -C web/remote test && pnpm -C web/remote exec tsc --noEmit && pnpm -C web/remote build` → PASS; `dist/manifest.webmanifest` has `"name":"openOrchestra"`; `cargo test -p oo-remote` passes.
- [ ] **Step 5:** Commit `feat(remote-web): openOrchestra branding, agent icons, app-style status, pair once on iOS`.

### Task 1: Transcript readers → `Entry` list

**Files:**
- Create: `crates/remote/src/conversation.rs`; fixtures `crates/remote/tests/fixtures/claude-transcript.jsonl` (hand-written, sanitized — no real user data) and reuse `crates/training/fixtures/pi-session-v3.jsonl` by copying it to `crates/remote/tests/fixtures/pi-session-v3.jsonl`
- Modify: `crates/remote/src/lib.rs` (module + re-exports)
- Test: inline `#[cfg(test)]` in `conversation.rs`

**Interfaces:**
- Produces (all `pub`, serde `rename_all = "snake_case"` / lowercase as shown):
  - `enum TranscriptKind { ClaudeCode, Pi }` + `fn TranscriptKind::from_agent_kind(kind: &str) -> Option<TranscriptKind>`
  - `struct Entry { id: String, kind: EntryKind, text: Option<String>, tool: Option<ToolLine>, duration_ms: Option<u64>, at: Option<String> }` (`#[serde(skip_serializing_if = "Option::is_none")]` on options)
  - `enum EntryKind { User, Assistant, Thinking, Tool, Notice }`
  - `struct ToolLine { id: String, name: String, summary: String, status: ToolStatus }`, `enum ToolStatus { Pending, Ok, Error }`
  - `fn parse_entries(kind: TranscriptKind, text: &str, cwd: Option<&Path>) -> Vec<Entry>` — active branch only, oldest first, tool statuses resolved from their results.
  - `fn tool_result(kind: TranscriptKind, text: &str, tool_id: &str) -> Option<ToolResult>`; `struct ToolResult { text: String, is_error: bool, truncated: bool }` (64 KiB cap).
  - `fn tool_summary(name: &str, input: &serde_json::Value, cwd: Option<&Path>) -> String`
- Entry ids: Claude `"<record uuid>:<block index>"`; pi `"<record id>:<block index>"`. Tool entries use the tool-use block's position (status changes keep the id).

- [ ] **Step 1: Write the fixture and failing tests:**
  - `claude_text_thinking_and_tools`: fixture gives in order: User("fix the test"), Thinking(text None, duration_ms Some(12000)), Tool{name "Bash", summary "cargo test -p oo-remote", status Ok}, Tool{"Edit", "tests/server.rs", Error}, Assistant("Fixed."). Thinking with text gets `text` ≤ 4096 bytes + `…[truncated]` when longer.
  - `claude_skips_sidechain_meta_and_bookkeeping`: sidechain, `isMeta`, `attachment`, `system`, `file-history-snapshot` records produce no entries; an `image` block produces Notice("[image]").
  - `claude_tool_result_user_record_is_not_a_prompt`: a `user` record whose content is only `tool_result` blocks yields no User entry.
  - `claude_follows_active_branch`: two children of one parent; entries follow the branch of the last user/assistant record.
  - `pi_fixture_entries`: the pi fixture yields User, Tool(status Ok), Assistant with timestamps in `at`.
  - `tool_summary_per_tool`: Edit/Write/Read `{"file_path": "/repo/src/a.rs"}` with cwd `/repo` → `"src/a.rs"`; Bash `{"command":"cargo test\nmore"}` → `"cargo test"`; a Bash command of 100 chars → first 80 + `…`; Grep `{"pattern":"send_keys"}` → `"\"send_keys\""`; WebFetch `{"url":"https://docs.rs/x/y"}` → `"docs.rs"`; unknown `{"q":"abc"}` → `"abc"`.
  - `tool_result_is_capped`: a 200 KiB result → 64 KiB text, `truncated: true`; unknown id → None.
  - `unknown_kinds_unsupported`: `from_agent_kind("codex") == None`.
- [ ] **Step 2:** `cargo test -p oo-remote conversation` → FAIL (missing).
- [ ] **Step 3:** Implement. Parse line by line with `serde_json::Value` (ignore unparsable lines). Active branch: build `id → parent` for user/assistant records, walk from the last one.
- [ ] **Step 4:** `cargo test -p oo-remote conversation` → PASS; clippy/fmt clean.
- [ ] **Step 5:** Commit `feat(remote): transcript readers for the conversation view`.

### Task 2: Shared transcript tailer

**Files:**
- Create: `crates/remote/src/transcript.rs`
- Modify: `crates/remote/src/lib.rs`
- Test: inline tests (tempfile)

**Interfaces:**
- Consumes: Task 1.
- Produces:
  - `const WINDOW: usize = 200; const READ_CAP: u64 = 8 * 1024 * 1024;`
  - `fn read_capped(path: &Path, cap: u64) -> io::Result<String>` — whole file if ≤ cap, else last `cap` bytes after the first newline.
  - `enum Update { Replace(Vec<Entry>), Upsert(Vec<Entry>) }` — `Replace` = full window (connect, reset, fork, new file); `Upsert` = changed-by-id + new entries in order.
  - `fn diff(prev: &[Entry], next: &[Entry]) -> Option<Update>` — `None` if equal; `Upsert` when every id of `prev` still appears in `next` in the same order; otherwise `Replace(window(next))`.
  - `struct Transcripts` (one per server, `Clone`): `fn get(&self, path: PathBuf, kind: TranscriptKind) -> Arc<Tail>` (shared via a `Weak` map like `Hubs`).
  - `struct Tail`: `fn subscribe(&self) -> (Vec<Entry> /*current window*/, tokio::sync::broadcast::Receiver<Update>)`; `fn tool_result(&self, tool_id: &str) -> io::Result<Option<ToolResult>>` (re-reads the capped file); background task watches the file (notify on the parent dir, filtered to the file name) plus a 1 s poll backstop; stops when the last `Arc<Tail>` drops.
  - A file that doesn't exist → empty window, keep polling. An unreadable file → window = `[Notice("Can't read the conversation right now")]`, keep retrying.

- [ ] **Step 1: Failing tests:**
  - `diff_upsert_on_append_and_status_change`: prev [A, T(pending)], next [A, T(ok), B] → `Upsert([T(ok), B])`.
  - `fork_after_send_resets`: prev [A, B], next [A, C] → `Replace([A, C])`.
  - `window_is_last_200`: 250 entries → Replace carries the last 200.
  - `read_capped_skips_partial_first_line`: 10-byte cap on `"aaaa\nbb\ncc\n"` returns `"bb\ncc\n"`… (choose a cap that cuts mid-line and assert the first partial line is dropped).
  - `tail_broadcasts_appended_lines` (tokio): write 2 records, subscribe, append 1 record → receive `Upsert` with the new entry within 2 s.
  - `missing_file_then_created`: path absent → empty window; create file with 1 record → `Replace`/`Upsert` with it within 2 s.
  - `tail_stops_when_dropped`: after dropping all `Arc<Tail>`, `Transcripts::get` returns a fresh tail (weak map cleared).
- [ ] **Step 2:** `cargo test -p oo-remote transcript` → FAIL.
- [ ] **Step 3:** Implement (blocking reads via `spawn_blocking`).
- [ ] **Step 4:** PASS; clippy/fmt clean.
- [ ] **Step 5:** Commit `feat(remote): shared transcript tailer`.

### Task 3: Conversation endpoints

**Files:**
- Modify: `crates/remote/src/server.rs` (`AgentInfo` fields; `Transcripts` in server state), `crates/remote/src/api.rs` (routes)
- Create: `crates/remote/src/conversation_api.rs` (handlers, keep api.rs small)
- Test: `crates/remote/tests/server.rs`

**Interfaces:**
- Consumes: Tasks 1–2; existing auth/marks helpers in api.rs (`session_for`-style lookup, `current_file`, closed-reason handling used by `/api/events`).
- Produces:
  - `AgentInfo` gains `#[serde(skip)] pub transcript: Option<PathBuf>` and `pub has_conversation: bool` (server sets it: `transcript.is_some() && TranscriptKind::from_agent_kind(agent_kind).is_some()`; the app need not compute it).
  - `GET /api/agents/{id}/conversation` (SSE, bearer auth): on connect `event: entries` data `{"replace":true,"entries":[…]}`; then `{"replace":false,"entries":[…]}` for `Upsert`, `{"replace":true,…}` for `Replace`; a changed transcript path for the id (snapshot update) → a fresh `replace:true` from the new tail; `event: closed` data `unmarked|revoked|disabled` then end; `:keepalive` every 15 s. 404 for unknown/unmarked id or `has_conversation == false`.
  - `GET /api/agents/{id}/conversation/tool/{tool_id}` → 200 `{"text","is_error","truncated"}`; 404 unknown tool/id.
- [ ] **Step 1: Failing tests** (temp transcript files; snapshot via the test `snapshot_channel`):
  - `conversation_streams_entries`: marked agent with a Claude fixture path → first event `replace:true` with the fixture's entries; appending a record → `replace:false` event within 2 s.
  - `conversation_requires_mark_and_support`: unmarked id → 404; marked id with `agent_kind "codex"` → 404 and `has_conversation:false` in `/api/agents`.
  - `transcript_path_never_sent`: the temp path string appears in no `/api/agents`, SSE or tool response body.
  - `new_transcript_path_replaces_stream`: change the snapshot's transcript path → next event `replace:true` with the new file's entries.
  - `unmark_closes_conversation`: unmark → `event: closed` `unmarked`.
  - `tool_endpoint_caps_output`: a 200 KiB tool result → 64 KiB `text`, `truncated:true`; unknown tool → 404.
  - `read_only_device_can_read_conversation`.
- [ ] **Step 2:** `cargo test -p oo-remote --test server conversation` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test -p oo-remote` and `-- --ignored` → PASS; clippy/fmt clean.
- [ ] **Step 5:** Commit `feat(remote): conversation stream and tool-result endpoints`.

### Task 4: App passes transcript paths

**Files:**
- Modify: `crates/harness-app/src/workspace/remote.rs` (`RemoteRuntime`, `build_snapshot`, `MarkSource`), `crates/harness-app/src/workspace/state/alerts.rs` (`apply_agent`), startup seeding in `remote_init`
- Test: inline tests in `remote.rs`

**Interfaces:**
- Consumes: `AgentInfo.transcript` (Task 3).
- Produces:
  - `RemoteRuntime.session_files: HashMap<(PathBuf /*worktree root*/, String /*agent kind*/), PathBuf>` and `RemoteRuntime.latest_session: HashMap<PathBuf, PathBuf>` (most recent per root, any kind).
  - `fn record_session(&mut self, root: &Path, kind: &str, file: &Path)` — called from `apply_agent` when `signal.session_file` is Some (kind = `signal.kind.as_str()`).
  - `build_snapshot(file, sources, status, since, session_files, latest_session) -> Snapshot`: an `Agent(kind)` tab gets `session_files[(root, kind)]`; a Shell tab (kind None) gets `latest_session[root]`.
  - Seeding: in `remote_init`, for each registry entry with a `session_file`, `record_session(worktree path, entry.agent_kind.as_str(), file)`.
- [ ] **Step 1: Failing tests:**
  - `agent_tab_gets_its_own_kind_transcript`: root R has claude-code → /c.jsonl and pi → /p.jsonl; a `claude-code` tab gets /c.jsonl, a `pi` tab /p.jsonl.
  - `shell_tab_gets_latest_transcript`: record claude-code then pi for R → Shell tab gets the pi path.
  - `no_transcript_when_unknown`: a codex tab with nothing recorded → `transcript: None`.
- [ ] **Step 2:** `cd crates/harness-app && cargo test remote::` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `cargo test remote:: && cargo build` → PASS; clippy clean on touched files.
- [ ] **Step 5:** Commit `feat(app): send each marked tab's transcript to the remote server`.

### Task 5: Phone conversation pane

**Files:**
- Create: `web/remote/src/lib/conversation.ts`, `web/remote/src/components/ConversationView.tsx`, `web/remote/src/components/EntryRow.tsx`, `web/remote/src/components/ToolSheet.tsx`, `web/remote/src/components/ui/tabs.tsx` (shadcn)
- Modify: `web/remote/src/lib/api.ts` (`Agent.has_conversation`, `fetchToolResult`), `web/remote/src/components/AgentView.tsx` (Conversation | Terminal switch)
- Test: `web/remote/src/lib/conversation.test.ts`, `web/remote/src/components/ConversationView.test.tsx`

**Interfaces:**
- Consumes: Task 3 wire shapes; existing `streamEvents` (sse.ts) with bearer auth and the existing `backoffMs` from `src/hooks/useAgents.ts`.
- Produces:
  - `type ConvEntry` (mirrors `Entry`), `function mergeEntries(list: ConvEntry[], incoming: ConvEntry[], replace: boolean): ConvEntry[]` (replace → incoming; else known id replaced in place, unknown appended; result capped to the last 200).
  - `function useConversation(id: string): { entries: ConvEntry[]; connected: boolean; closed?: string }` (reconnect with backoff; `closed` handling like the terminal: `revoked` → clear token).
  - `fetchToolResult(id, toolId): Promise<{text: string; is_error: boolean; truncated: boolean}>`.
- Behaviour: user entries right-aligned bubbles; assistant entries left, Markdown rendered with a library that escapes raw HTML (no `dangerouslySetInnerHTML` of unsanitized input); tool entries one line `{icon} {summary}` with ✓/✗/spinner for ok/error/pending, tap → `ToolSheet` (bottom sheet, monospace, scrollable, `…[truncated]` note); thinking collapsed (`💭 Thinking…` + first line / `💭 Thought for {round(ms/1000)} s` / `💭 Thinking (not recorded)`), tap expands when text exists; notice entries centered muted text. Sticks to bottom unless scrolled up (> 80 px from bottom) → `New messages ↓` pill that scrolls down. Header switch `Conversation` | `Terminal` (shadcn Tabs, ≥ 44 px), default Conversation when `has_conversation`, else Terminal with the switch hidden; choice saved in `localStorage` `oo.view.<id>` (try/catch). The terminal socket only connects while the Terminal tab is shown.
- [ ] **Step 1: Failing tests:**
  - `mergeEntries`: replace; upsert in place keeps order; unknown appended; cap 200.
  - `renders each kind`: user bubble text, assistant Markdown (`**b**` → `<strong>`), `<script>` in assistant text rendered as text not HTML, tool line `✓ cargo test`, thinking variants (three copy strings).
  - `tap tool opens sheet with fetched output` (mock fetch).
  - `does_not_jump_when_scrolled_up`: scrolled up + new entries → scrollTop unchanged and pill visible; tapping pill scrolls to bottom.
  - `view switch`: has_conversation → Conversation default; false → Terminal and no switch; choice persisted.
- [ ] **Step 2:** `pnpm -C web/remote test` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `pnpm -C web/remote test && pnpm -C web/remote exec tsc --noEmit && pnpm -C web/remote build` → PASS.
- [ ] **Step 5:** Commit `feat(remote-web): conversation view`.

### Task 6: Live status bar

**Files:**
- Create: `web/remote/src/components/StatusBar.tsx`
- Modify: `web/remote/src/components/ConversationView.tsx` (bar between list and reply box), `web/remote/src/components/AgentView.tsx` (focus reply box on waiting)
- Test: `web/remote/src/components/StatusBar.test.tsx`

**Interfaces:**
- Consumes: the agent's live `status` and `tool` from `useAgents` (already streamed).
- Behaviour: `running` → looping animation (three dots bouncing in sequence, CSS keyframes) + `Working…` + `tool` summary when present; `prefers-reduced-motion: reduce` → a static dot instead of the animation; `waiting` → amber `Needs your input` and the quick-reply input is focused once per transition into waiting; `idle`/`unknown` → hidden; `stopped` → `Agent stopped`. `role="status"` + `aria-live="polite"`.
- [ ] **Step 1: Failing tests:** each state's text; animation element present for running and absent under reduced motion (mock `matchMedia`); waiting focuses the reply input once (not on every re-render); hidden when idle.
- [ ] **Step 2:** `pnpm -C web/remote test` → FAIL.
- [ ] **Step 3:** Implement.
- [ ] **Step 4:** `pnpm -C web/remote test && tsc --noEmit && build` → PASS; `cargo test -p oo-remote` still passes (embedded dist).
- [ ] **Step 5:** Commit `feat(remote-web): live status bar for the conversation view`.

### Task 7: Docs and checklist

**Files:**
- Modify: `README.md` (Remote section: one paragraph on the Conversation view and which agents support it), `docs/superpowers/plans/2026-11-16-oo-remote-manual-checklist.md` (new section G: Conversation view — Claude Code and pi; tool tap; thinking lines; status bar loop + reduced motion; scroll-up pill; rewind in Claude → redraws; new session → switches; Codex tab shows Terminal only; also fix line 39: a full-tunnel VPN's address is expected, split-tunnel/VM bridges use the Wi-Fi address)
- [ ] **Step 1:** Edit the docs. **Step 2:** Run `cargo test -p oo-remote`, `pnpm -C web/remote test`, `(cd crates/harness-app && cargo test remote::)` and paste summary lines in the commit body. **Step 3:** Commit `docs: conversation view usage and checklist`.
