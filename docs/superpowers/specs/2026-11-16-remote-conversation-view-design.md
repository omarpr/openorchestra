# oo-remote — conversation view on the phone (design)

**Date:** 2026-11-16 · **Status:** draft for approval · **Builds on:** `2026-11-16-oo-remote-design.md` (branch `feat/oo-remote`)

## Intent

On a phone, the live terminal is a shrunken copy of the desktop screen —
hard to read. The user wants to follow an agent as **readable text**: what
they asked, what the agent answered, what it is doing — and still reply.

## Goals

1. A **Conversation** view per marked agent, for agents that expose a
   transcript (Claude Code and pi today), alongside the existing Terminal view.
2. Messages verbatim (no AI summarising): user prompts and agent replies in
   full (agent replies rendered as Markdown).
3. Tool calls as one line each (`✎ Edit src/main.rs`, `$ cargo test`,
   `🔍 Grep "send_keys"`) with ✓ / ✗ once their result is in; **tap a tool
   line to load that one result** on demand.
4. Thinking shown **collapsed** (`💭 Thinking…` + first line, tap to expand);
   when the transcript holds no thinking text, show `💭 Thought for 12 s`
   (from the recorded duration) or `💭 Thinking (not recorded)`.
5. A **live status bar** at the bottom so the user knows the agent is working:
   running → a looping animation + "Working…" + the current tool;
   waiting → amber "Needs your input" and the reply box focused;
   idle → hidden; stopped → "Agent stopped".
6. The existing quick-reply box under the conversation.

## Non-goals

- AI summaries; editing or deleting messages; showing subagent (sidechain)
  conversations; images/attachments (shown as a placeholder line); a
  conversation view for Codex / OpenCode / Kimi (they keep the Terminal view).
- Streaming partial replies: agents write a message only when it is complete;
  the status bar covers the gap.

## Anti-goals

- The phone choosing which file is read — the transcript path comes **only**
  from the Mac app's snapshot for a currently marked agent.
- Shipping whole tool outputs or the full transcript to the phone by default.
- Parsing or file I/O on the GPUI main thread.

## Approaches considered

1. **Mac parses and streams trimmed entries (chosen).** App passes each marked
   agent's transcript path in the snapshot; oo-remote parses (pi reader reused,
   new Claude Code reader), tails the file, and streams entries.
2. Phone parses raw JSONL — duplicate pi parser in TS and full tool output /
   thinking over Wi-Fi. Rejected.
3. Desktop app parses into the snapshot — parsing and big updates on the UI
   path. Rejected.

## Facts this builds on

- `AgentSignal.session_file` (agent-runtime) carries the transcript path for
  Claude Code (hook `transcript_path`) and pi. The app stores it per worktree
  in the registry only for worktrees (`alerts.rs:181-198`), not main
  checkouts. Status (and therefore the transcript) is **per worktree**: every
  marked tab of a worktree shows that worktree's latest agent session.
- pi reader: `crates/training/src/session.rs` (`parse_session`, active-branch
  resolution via `parentId`). It depends only on serde, but the `training`
  crate pulls `worktrees`/`forge`/`agent-schema`.
- Claude Code JSONL (observed in a real transcript): records with `type`
  `user` / `assistant` (plus many bookkeeping types to ignore), `uuid`,
  `parentUuid`, `isSidechain`, `isMeta`, `timestamp`, `message.content` blocks:
  `text`, `thinking` (usually **empty text**; `thinkingDurationMs` on the
  record), `tool_use` (`id`, `name`, `input`), `tool_result` (`tool_use_id`,
  `content`, `is_error`), `image`. A user record whose content is only
  `tool_result` blocks is a tool result, not a user prompt.

## Design

### 1. Data flow
- **App** (`workspace/remote.rs`): keep `session_files: HashMap<PathBuf /*worktree root*/, PathBuf>`
  updated in `apply_agent` from `signal.session_file` (covers main checkouts);
  add `transcript: Option<PathBuf>` and `agent_kind` to each `AgentInfo` in the
  snapshot (`#[serde(skip)]` for the path — never sent to the phone).
- **Server** (oo-remote): new module `conversation.rs` with readers for
  Claude Code and pi producing a common `Entry` list; a per-transcript
  **tailer** (shared across viewers, like `Hubs`) that re-reads appended lines
  on file change (notify + 1 s poll backstop) and publishes new entries.
- **API** (authenticated, marked-only, re-checked against the current file
  like the terminal):
  - `GET /api/agents/:id/conversation` (SSE): `event: entries` with the last
    200 entries on connect, then appended entries; `event: reset` if the
    transcript path changes (new session) or the file shrinks.
  - `GET /api/agents/:id/conversation/tool/:tool_id` → that tool result's text,
    capped at 64 KiB (truncated with a marker), 404 if unknown.
  - `AgentInfo` gains `has_conversation: bool` (true when a supported
    transcript path is known).

### 2. Entry shape (wire)
```json
{ "id": "<stable>", "kind": "user" | "assistant" | "thinking" | "tool" | "notice",
  "text": "…",                                   // user/assistant/thinking/notice
  "tool": { "id": "toolu_…", "name": "Edit", "summary": "src/main.rs",
            "status": "pending" | "ok" | "error" },
  "duration_ms": 12000,                           // thinking, when recorded
  "at": "RFC3339" }
```
- Tool summary per tool name (pure, tested): Edit/Write/Read → file path
  (relative to cwd when inside it); Bash → command (first line, ≤ 80 chars);
  Grep/Glob → pattern; WebFetch → URL host; unknown → first string input.
  When a `tool_result` arrives, the tool entry's status updates (sent as a
  replacement entry with the same id).
- Thinking `text` truncated to 4 KiB; assistant/user text uncapped up to
  64 KiB per message (truncated with a marker beyond).
- Skip: sidechain records, `isMeta`, bookkeeping types, empty text blocks.
  Active branch: for Claude Code, follow `parentUuid` from the last
  user/assistant record (same idea as the pi reader); for pi reuse the training
  reader's branch resolution.

### 3. Phone UI (web/remote)
- Agent view header gets a **Conversation | Terminal** segmented control
  (shadcn Tabs), defaulting to Conversation when `has_conversation`, else
  Terminal (control hidden). Choice remembered per agent.
- Conversation pane: scrollable list, auto-sticks to bottom unless the user
  scrolled up (then a "New messages ↓" pill). User messages right-aligned
  bubbles; agent replies left, Markdown (lightweight renderer, code blocks
  monospace and horizontally scrollable); tool lines compact, tap → bottom
  sheet with the fetched output (monospace, scrollable); thinking collapsed.
- **Status bar** above the reply box, from the existing SSE agent status:
  running → looping animation (three bouncing dots / shimmer, respects
  `prefers-reduced-motion` with a static dot) + "Working…" + current tool
  summary; waiting → amber "Needs your input" and focus the reply box;
  idle → hidden; stopped → "Agent stopped".
- Phone and tablet layouts as today (tablet: right pane).

### 4. Security
- Transcript path never from the client and never sent to it; only for ids in
  the current marks; re-checked on every request and when marks change
  (unmark → stream closes with `unmarked`, same as the terminal).
- Read-only devices may read the conversation; reply box hidden as today.

### 5. Testing
- Readers: fixture JSONL (sanitized, committed) for Claude Code covering
  text, empty/non-empty thinking, tool_use + tool_result (ok and error),
  sidechain and meta skipped, image placeholder, a fork (active branch);
  pi via the existing fixture.
- Tailer: appended lines produce only new entries; truncation/replacement
  → reset; tool status update replaces the entry.
- API: unmarked id → 404; path never appears in any response; tool result
  cap; read-only device can read.
- Web: entry rendering per kind, sticky-bottom behaviour, status bar states
  incl. reduced motion, tab default/memory.

## Decisions from the builder check (my calls — marked so they can be revisited)

1. **pi reader:** write a small incremental pi reader inside oo-remote's
   `conversation.rs` (timestamps, ids, thinking if present, tool calls) and
   leave `crates/training` untouched — the harvester's `Turn` is shaped for
   training, not display. Thinking lines apply to any agent whose transcript
   records them.
2. **Branch changes:** on each file change the tailer re-resolves the active
   branch (parse is cheap at the size cap below). If the entries already sent
   are still a prefix of the new branch, it sends only the new/changed
   entries; otherwise it sends `reset` followed by the fresh window.
3. **Which transcript a tab shows:** `session_files` is keyed by
   (worktree root, agent kind). An `Agent(kind)` tab gets the transcript of
   its own kind only. A `Shell` tab is Terminal-only. When a worktree has
   more than one open tab of the same kind, all of them are Terminal-only
   (the one file on record could be either's). Opening or closing an agent
   tab of a kind forgets that kind's file for the worktree, so a new tab
   shows Terminal until its first signal records a fresh one. At startup it
   is seeded from the registry (worktrees); main checkouts get one on the
   agent's first signal. `has_conversation` is true once a path is known
   for that tab.
4. **Wire contract:** every connect (and reconnect) sends a full snapshot
   (`entries` with `replace: true`), so the client replaces its list — no
   `Last-Event-ID` replay. Later `entries` events are merged **by id**: a known
   id replaces in place (tool status updates), an unknown id appends.
   Window = last 200 entries; no "load earlier" in this version (a long
   session's beginning is not shown — recorded limit).
5. **Tool results on tap:** the endpoint re-reads the transcript and finds the
   result by `tool_id` (unique per file in both formats); any tool in the file
   can be fetched, not just those in the window. Capped at 64 KiB.
6. **Missing/unreadable transcript:** a known path whose file doesn't exist
   yet → empty list, the stream waits for it; unreadable → a single `notice`
   entry ("Can't read the conversation right now") and the stream keeps
   retrying. Only unmark/revoke/disable close the stream.
7. **Large files:** read at most the last 8 MiB of the file (seek from the
   end, skip the first partial line); branch resolution works within that.
8. **Web dependencies:** a small Markdown dependency is allowed if it escapes
   raw HTML (no HTML passthrough); shadcn Tabs is added if not present.

## Left to the builder
Markdown renderer choice (small, no HTML passthrough), notify vs poll details,
exact animation, how the pi reader is shared (move `session.rs` into a tiny
crate, or depend on `training` if it stays light), module layout.
