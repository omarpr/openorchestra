# Multi-context tasks with A2A orchestration

**Date:** 2026-08-09
**Status:** Implemented (MVP)
**Scope owner:** openOrchestra / worktrees + agent-runtime + harness-app + a2a-broker

## Goal

Multi-member tasks stop relying on one home-cwd agent that is *told* about
sibling paths. Each included project gets its own agent context (tabs + cwd).
openOrchestra brokers Agent2Agent (A2A) messaging so the home agent can
discover and delegate to specialists.

## Terminology map (critical)

| openOrchestra | A2A v1.0 | Notes |
|---|---|---|
| Task (`tasks.json`) | `contextId` | One effort spanning N repos |
| Task member worktree | Agent (specialist) + tenant | One Agent Card per member |
| Home member | Orchestrator client | Primary user chat; A2A client via tools |
| Delegation / work unit | A2A `Task` | States: submitted → working → completed/failed/canceled/input_required |
| Unified DIFF | (product surface) | Still OO-level; specialists may attach file artifacts |

Never call an A2A Task an “openOrchestra task” in UI copy.

## Agent Card (per member)

Published at `http://127.0.0.1:<port>/.well-known/agent-card.json` (and
tenant-scoped cards under `/t/<worktree_id>/.well-known/agent-card.json`).

```json
{
  "name": "openOrchestra · <repo>",
  "description": "Specialist for <repo> worktree in task <slug>",
  "version": "1.0",
  "protocolVersion": "1.0",
  "url": "http://127.0.0.1:<port>/t/<worktree_id>/jsonrpc",
  "preferredTransport": "JSONRPC",
  "skills": [
    {
      "id": "repo-specialize",
      "name": "Edit & review <repo>",
      "description": "Implement and review changes only in this worktree.",
      "tags": ["repo:<repo>", "role:specialist"]
    }
  ],
  "capabilities": { "streaming": true },
  "securitySchemes": {
    "apiKey": { "type": "apiKey", "in": "header", "name": "Authorization" }
  },
  "security": [{ "apiKey": [] }],
  "defaultInputModes": ["text/plain"],
  "defaultOutputModes": ["text/plain"]
}
```

Home orchestrator card uses skill `role:orchestrator` and lists sibling card URLs
in the harness-owned `agent-context.md`.

## Bridge fidelity matrix

Coding CLIs do not speak A2A. The broker is always the A2A Server facade.

| AgentKind | Enqueue path | Completion signal | Fidelity |
|---|---|---|---|
| Pi | Inbox file + optional tmux inject; pi harness tool when present | Outbox / status Idle after working | BestEffort → Exact with tool |
| Claude Code | Inbox file + tmux inject; MCP later | Status file Idle | BestEffort |
| Codex | Inbox + tmux | notify turn-complete | BestEffort |
| OpenCode | Inbox + tmux | SSE session.idle | BestEffort |
| Kimi / Other | Inbox + tmux | None (manual / timeout) | TerminalOnly |

MVP bridge: **file inbox/outbox** under
`~/.openorchestra/tasks/<slug>/a2a/` plus optional tmux `send-keys`. Preferred
upgrade: adapter MCP/tools that call broker `complete`.

## Security (v1)

- Bind `127.0.0.1` only.
- Per-task bearer API key in Agent Card + `Authorization: Bearer <key>`.
- Opaque execution: agents exchange Messages/Artifacts only.
- Audit JSONL: `~/.openorchestra/tasks/<slug>/a2a/audit.jsonl`.

## Pane keys

- Before: `task:{task_id}` (one pane set; agent cwd = home).
- After: `task:{task_id}:member:{worktree_id}` (per-member Agent/Shell/Editor).
- Migrate persisted `panes.json` keys: old `task:{id}` → home member key on load.
- Unified DIFF remains task-scoped (not per-member keyed).

## Guided start

On create (or first open) of multi-member tasks, show “Start specialists”:
one row per member with “Open Agent”. Home gets orchestrator context; siblings
get specialist context.

## Non-goals (this delivery)

- Push-notification webhooks
- Multi-root Zed Project
- Full a2a-rs dependency (minimal JSON-RPC subset in `crates/a2a-broker`)
- Cross-org / non-localhost agents

## Success criteria

1. Per-member Agent tabs with correct cwd.
2. Home agent delegates via A2A without reading sibling trees from home cwd.
3. openOrchestra owns discovery, auth, task state, and audit.
