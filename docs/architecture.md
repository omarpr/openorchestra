# omar-harness — Architecture Document

**Status:** Draft v2 · July 19, 2026
**Scope:** A native Rust desktop harness for running our own trained coding agent across parallel git worktrees, with embedded Zed-grade editing and terminals.
**v2 changes:** Integrated the `Harness.dc.html` design (Claude Design project "Coding Agent Harness UI"); §10 open questions resolved into decisions (§10); agent interaction re-phased — v1 embeds pi's TUI in the terminal, the custom RPC chat panel becomes Phase 2 (§4); added forge/CI integration (§6.3) and UI spec (§3.4).

---

## 1. Purpose and goals

We are building an agentic development harness with three defining properties:

1. **Our model, our loop.** The agent is a model we train ourselves on the behaviors that matter to us. The harness must let us control the tool schema, system prompt, and conversation format end-to-end, so the harness conventions and the training data are the same thing.
2. **Worktree-native parallelism.** The unit of work is a git worktree. Each worktree gets its own agent session, editor, and terminal. Running N tasks means N isolated worktrees, visible side by side — across multiple repos from day one.
3. **Native UI, real editor.** The shell is a Rust app built on GPUI (Zed's UI framework), embedding Zed's editor and terminal crates directly — actual panes inside our app, not orchestrated external windows.

Non-goals for v1: collaboration/multiplayer, remote development, Windows support (macOS first, Linux second), plugin ecosystem, non-GitHub forges, custom agent chat UI (Phase 2 — see §4.1).

## 2. System overview

```
┌────────────────────────────────────────────────────────────────────┐
│  Harness app (Rust, GPUI) — single window                          │
│  ┌──────────────┬──────────────────────────────┬─────────────────┐ │
│  │ WORKTREES    │  Tab strip (per worktree)    │ DIFF panel      │ │
│  │ sidebar      │  [❯_ feat/wt-nav][{} card.rs]│ changed files   │ │
│  │  repo A      │ ┌──────────────────────────┐ │ +38 −8          │ │
│  │   ◦ main     │ │ Terminal tab:            │ │ per-file diff   │ │
│  │   ◦ wt: nav  │ │  ── pi TUI runs here ──  │ │ view            │ │
│  │     [RUNNING]│ │  (v1 agent chat surface) │ │                 │ │
│  │   ◦ wt: diff │ ├──────────────────────────┤ │                 │ │
│  │     [WAITING]│ │ Editor tab (Zed editor   │ │                 │ │
│  │  repo B      │ │  crate, rust-analyzer)   │ │                 │ │
│  │   ◦ …        │ └──────────────────────────┘ │                 │ │
│  │ [+ new]      │  status bar: agent · branch  │                 │ │
│  └──────────────┴──────────────────────────────┴─────────────────┘ │
│                                                                    │
│  Worktree manager ── Forge poller (gh) ── Status watcher (v1)      │
│                       Agent supervisor ── RPC client (Phase 2)     │
└──────────┬───────────────────┬─────────────────────────────────────┘
           │                   │
     git worktree        ┌─────┴─────────┐   v1: pi TUI in embedded pty
     add/remove          │ pi (Node.js)  │   P2: also `pi --mode rpc`
     gh pr/checks        │ per worktree  │        headless sidecars
                         └─────┬─────────┘
                               │ OpenAI-compatible HTTP
                         ┌─────┴───────────────┐
                         │ LM Studio (local)   │ ← our trained model
                         │ localhost:1234/v1   │   + cloud baselines
                         └─────────────────────┘
```

Three subsystems, cleanly separable:

- **Shell (Rust/GPUI):** windowing, panes, editor, terminal, sidebar, diff panel.
- **Agent runtime (pi, TypeScript):** the agent loop, tools, session persistence, provider layer. In v1 it runs as its own TUI inside our terminal panes; in Phase 2 additionally as headless RPC sidecars. We do not reimplement the loop.
- **Model serving (LM Studio):** OpenAI-compatible endpoint serving our trained model locally. Swappable for vLLM/SGLang later without touching the harness.

## 3. The shell: GPUI + Zed crates

### 3.1 Framework choice

GPUI is Zed's GPU-accelerated UI framework, licensed **Apache-2.0** and usable standalone. Not published as a stable crates.io release; we depend on a **pinned git revision** of `zed-industries/zed` and upgrade deliberately, keeping gpui and the editor crates on the same revision.

### 3.2 Editor panes

We reuse Zed's `editor` crate (and its dependency cone: `project`, `language`, `buffer`/`text`, `settings`, `theme`, tree-sitter grammars) for production-grade editor tabs: syntax highlighting, LSP (the design's status bar shows `rust-analyzer` — LSP is an explicit requirement), multibuffer diffs. Highest-risk dependency (§8 R1), gated by Spike 3.

| Crate | License | Role |
|---|---|---|
| `gpui` | Apache-2.0 | UI framework — safe to build on |
| `editor` | GPL-3.0-or-later | Editor pane |
| `project` | GPL-3.0-or-later | Worktree/file/LSP model |
| `language` | GPL-3.0-or-later | Syntax, tree-sitter, language config |
| `text` | GPL-3.0-or-later | Rope/buffer primitives |
| `terminal`, `terminal_view` | GPL-3.0-or-later | Zed's terminal wrapper over alacritty |
| `alacritty_terminal` (crates.io) | Apache-2.0 | Terminal emulation core |

**Licensing consequence:** linking any GPL-3.0 Zed crate makes the harness binary GPL-3.0. Decision recorded: acceptable (internal tool; open-sourcing under GPL is an option, selling proprietary licenses is not). Fallback path in §8 R1 confines the exposure.

### 3.3 Terminal panes

Zed's `terminal`/`terminal_view` crates (already GPL alongside the editor, GPUI-native, consistent rendering). **In v1 the terminal is also the agent chat surface** (§4.1): each worktree's primary terminal tab runs pi's own TUI. Additional plain-shell terminals via ⌘T. The design's diff panel carries an "alacritty" tag — consistent with this stack.

### 3.4 UI spec (from `Harness.dc.html`)

The design is the source of truth for v1 chrome; key commitments extracted:

- **Single window, three columns.** Left: worktree sidebar (resizable, 220–560px, collapsible). Center: tab strip + content + status bar. Right: diff panel (resizable, 260–640px, collapsible). Top bar: panel toggles, `harness · <active-branch>` breadcrumb, theme toggle.
- **Worktree sidebar.** Grouped by repo (multi-repo day one), each group: `main` entry (default branch, CI check) + worktree cards. Card anatomy: editable avatar (2-letter initials + color from 8-swatch palette), inline-renamable name, mono branch label, PR badge (`#N` — open/draft/merged/closed colors), ahead/behind counts, dirty indicator (`~N` files), CI status (pass/fail/running), agent status pill (**Running** pulse / **Waiting** amber / **Idle** gray), hover actions: archive, delete.
- **Tabs are scoped per worktree** — each worktree remembers its own tab set (terminals + editors); switching worktree cards swaps the strip. Tab menu: New terminal ⌘T, New editor ⌘E, Agent chat (Phase 2).
- **Diff panel.** Collapsible changed-files list (per-file status M/A/D badge, +adds/−dels, totals) above a scrollable unified diff view with sticky file header. Driven by git status/diff against the worktree's merge base; later this is also where agent-produced changes get reviewed (multibuffer integration candidate in Phase 2).
- **Status bar.** Agent state (mirrors the pill), branch, encoding, LSP server indicator.
- **Theming.** Dark + light palettes with a configurable accent color; fonts IBM Plex Sans / IBM Plex Mono. Design tokens (`--bg0..2`, `--surface*`, `--tx*`, `--accent`, semantic green/red/amber/purple, diff add/del tints) become the harness `Theme` struct; Zed editor/terminal crate themes are generated from the same tokens so panes match the chrome (mapping effort noted in §8 R6).

## 4. Agent runtime: pi per worktree

### 4.1 Phased interaction model

**Phase 1 (v1): pi TUI in the embedded terminal.** Each worktree's primary terminal tab runs `pi` interactively — chat, steering, approvals, /commands all come from pi's mature TUI at zero UI cost. This matches the design (its tab menu explicitly marks "Agent chat" as *soon*, and its terminal mock shows agent activity). The harness's v1 responsibilities shrink to: spawn pi in a pty with `cwd` = worktree root, keep session files organized per worktree, and surface agent state in the sidebar.

**Agent status without RPC:** a small pi **extension** (`pi-ext/harness-status.ts`, loaded per project) subscribes to agent lifecycle events and writes `{state, timestamp, sessionFile}` to `.harness/status.json` in the worktree on `agent_start` (→ running), UI dialog open (→ waiting), and `agent_settled` (→ idle). The Rust **status watcher** file-watches these to drive the sidebar pills and status bar. Same mechanism can later append outcome metadata for training-data harvesting.

**Phase 2: custom agent chat panel over RPC.** When "Agent chat" graduates from "soon," the agent supervisor additionally spawns `pi --mode rpc` sidecars and the chat tab renders the event stream natively (streaming deltas, tool cards, session tree, native approval modals). Everything in §4.2–4.3 is the contract for that phase — it's specified now so Phase 1 decisions (session layout, extension architecture, status schema) stay compatible with it.

### 4.2 IPC contract (Phase 2)

pi's RPC mode is JSONL over stdin/stdout with strict framing: **split records on `\n` only** (accept a trailing `\r`; do not treat U+2028/U+2029 as newlines). Commands may carry an `id` for correlation; responses echo it. Events carry no `id`.

Commands: `prompt` (with `streamingBehavior: "steer" | "followUp"` when streaming), `steer`, `follow_up`, `abort`, `new_session`, `switch_session`, `fork`, `get_tree`, `get_state`, `get_session_stats`, `get_entries` (with `since` cursor — the durable resume primitive across harness restarts), `set_model`, `set_thinking_level`, `compact`, `set_auto_compaction`, `set_auto_retry`.

Event stream: `agent_start` → `turn_start` → `message_start` → `message_update` (deltas: `text_delta`, `thinking_delta`, `toolcall_*`) → `tool_execution_start/update/end` → `turn_end` → `agent_end` (`willRetry`) → **`agent_settled`** (the true "done" signal). Plus `queue_update`, `compaction_start/end`, `auto_retry_start/end`, `extension_error`.

Rust-side: a `pi-client` crate with serde types mirroring `rpc-types.ts`, tokio task per process doing framed reads, events fanned out on a bus GPUI entities subscribe to. Conformance tests replay recorded JSONL from real pi runs. Pin the pi version; treat protocol upgrades like schema migrations.

### 4.3 Extension-UI sub-protocol (Phase 2)

Dialog methods (`select`, `confirm`, `input`, `editor`) arrive as `extension_ui_request` and block the agent until a matching-`id` `extension_ui_response` (or agent-side timeout) — these become native modals. Fire-and-forget methods (`notify`, `setStatus`, `setWidget`, `setTitle`) map to toasts/status-bar entries. In Phase 1, pi's own TUI handles all of this; the "Waiting" pill is driven by the status extension.

### 4.4 Customizing the agent for our model

Why pi: the loop is minimal and everything we need to co-evolve with training is pluggable without forking, via TypeScript extensions loaded per-project or per-user:

- `pi.registerTool()` / `pi.setActiveTools()` — make the runtime toolset exactly match our training tool schema (names, arg shapes, result formats). Harness-specific tools (worktree-aware ops, "request review") are extensions too, as is the status extension (§4.1).
- System prompt and prompt templates are overridable; the SDK (`createAgentSession`) is the escape hatch if we outgrow the stock CLI entry point. pi is MIT — forking is a supported failure mode, not a crisis.

**Contract discipline:** the tool schema is versioned in this repo as the single source of truth; the training-data generator and the pi extension both import from it. Model checkpoints declare their schema version; the harness refuses (or adapts) on mismatch.

## 5. Model serving

LM Studio serves the trained model on `http://localhost:1234/v1` (OpenAI-compatible, tool calling supported). Registered in `~/.pi/agent/models.json`:

```json
{
  "providers": {
    "lmstudio": {
      "baseUrl": "http://localhost:1234/v1",
      "api": "openai-completions",
      "apiKey": "lmstudio",
      "compat": { "supportsDeveloperRole": false, "supportsReasoningEffort": false },
      "models": [
        { "id": "omar-agent-v1", "name": "Omar Agent v1", "reasoning": true,
          "contextWindow": 131072, "maxTokens": 16384,
          "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 } }
      ]
    }
  }
}
```

Keyless local servers still need a dummy `apiKey`; verify `compat` flags against LM Studio's current behavior during Spike 2. Frontier models stay configured alongside as baselines — free A/B via pi's `/model`, same harness, same tools. Migration to vLLM/SGLang is a `baseUrl` change.

## 6. Worktree orchestration

### 6.1 Lifecycle

- **Creation:** `git worktree add <harness-dir>/worktrees/<repo>/<task-slug> -b <branch>` from a chosen base ref. One task = one worktree = one branch = one pi session. On open, the primary terminal tab spawns pi; the user types the task in pi's TUI.
- **Archive** (decision): remove the worktree directory, **keep** the branch, PR ref, and session JSONL. This is the default end-of-task action — sessions are training data (§6.4).
- **Delete** (decision): destroy worktree, local branch, and session, behind a confirm. For abandoned experiments only.
- **Registry:** on-disk index mapping worktree → repo, branch, session file, avatar (initials + color), status, PR number, created-at. Renames and avatar edits (per the design) mutate the registry, never git state.

### 6.2 Status

Sidebar card state is derived from: the status watcher (§4.1: running/waiting/idle), git (dirty file count, ahead/behind vs upstream), and the forge poller (§6.3: PR state, CI).

### 6.3 Forge integration (new in v2)

PR badges and CI status come from **GitHub via the `gh` CLI** (decision): a poller runs `gh pr view --json number,state,isDraft` and `gh pr checks` per worktree branch (and default-branch checks per repo) on a modest interval with jitter, parsing JSON output. Zero auth work — reuses the user's `gh` login. GitHub-only for v1; the poller sits behind a small trait so a direct-API or multi-forge backend can replace it without UI changes. `gh pr create` from the harness is a v1.x candidate.

### 6.4 Session exhaust as training data

Every session is an append-only JSONL tree we control the reading of. Archived sessions + final diffs + outcome labels (merged/abandoned — derivable from the forge poller) are the raw material for the next training round. The registry records enough metadata to make harvesting trivial. This closes the loop that motivates the whole project.

## 7. Repository and crate layout

```
omar-harness/
├── Cargo.toml            # workspace
├── crates/
│   ├── harness-app/      # GPUI app: sidebar, tabs, diff panel, theme
│   ├── worktrees/        # git worktree manager + registry (no UI deps)
│   ├── forge/            # gh-CLI poller behind a Forge trait (no UI deps)
│   ├── status-watch/     # .harness/status.json file watcher (no UI deps)
│   └── pi-client/        # Phase 2: RPC protocol types + framed stdio client
├── pi-ext/               # TypeScript: harness-status extension, custom tools,
│                         #   prompt templates, models.json template
├── agent-schema/         # tool schema source of truth (JSON Schema / Rust types)
├── training/             # schema-versioned training-data generation
└── docs/                 # this doc; design ref: claude.ai/design "Coding Agent Harness UI"
```

Non-UI crates are deliberately headless so the same backend can drive batch evals of new checkpoints (pi RPC + registry, no window).

## 8. Risks and mitigations

| # | Risk | Impact | Mitigation |
|---|---|---|---|
| R1 | Zed `editor` crate drags a huge dep cone, unstable internal APIs, no semver | Spike 3 fails or upgrade cost is chronic | Pin one zed revision; isolate behind an `EditorPane` trait; fallback = custom GPUI editor pane (rope + tree-sitter), keeping terminal + everything else intact |
| R2 | GPL-3.0 propagation to the harness binary | Constrains distribution/commercialization | Accepted, recorded in §3.2; permissive fallback path via R1 + alacritty_terminal |
| R3 | pi protocol/behavior drift across versions | Status extension or (P2) RPC client breaks | Pin pi version; conformance tests on recorded sessions; upgrades as migrations |
| R4 | Node.js runtime dependency per worktree | Distribution weight, memory | Acceptable (dev tool); ptys spawn lazily; long-term option: bundle Node |
| R5 | Our model's tool-calling reliability behind openai-compat servers | Loop degrades (malformed calls) | Schema-versioned contract (§4.4); constrained decoding in LM Studio/vLLM; frontier baseline always configured |
| R6 | GPUI docs thin; design-token → Zed theme mapping is bespoke | Slow UI progress, mismatched panes | Zed source as documentation; generate Zed crate themes from the harness token set early (part of Spike 3) |
| R7 | `gh` CLI polling: rate limits, output-format drift, gh not installed | Stale/broken badges | Poll with backoff + jitter; `--json` output is stable-ish; detect missing gh and degrade gracefully (badges hidden) |
| R8 | v1 status via file-watch is coarser than RPC events | Waiting-state fidelity (e.g., missed dialog states) | Keep the status extension minimal and evented (not polled); Phase 2 replaces it with the event stream |

## 9. Spike plan (order matters)

1. **Spike 1 — GPUI shell + terminal + pi.** Pinned zed rev; window with the three-column skeleton; one terminal tab running pi's TUI against LM Studio in a real worktree. *This now proves the entire v1 agent path* (was Spikes 1+2).
2. **Spike 2 — Status + forge pipeline.** `harness-status.ts` extension → `.harness/status.json` → Rust watcher → sidebar pill; `gh` poller → PR/CI badges. Proves the "mission control" layer.
3. **Spike 3 — Zed editor pane + theme mapping.** Real buffer with highlighting + rust-analyzer inside our app from the same pinned rev; generate editor/terminal themes from the design tokens. **Go/no-go for §3.2**; failure triggers R1 fallback.
4. **Spike 4 — Worktree vertical slice.** Sidebar with two repos / three worktrees, per-worktree tab sets, create/archive/delete flows, diff panel on real git state.

1 and 2 can run in parallel; 3 gates the editor decision; 4 integrates into the MVP skeleton, then chrome is styled to `Harness.dc.html`.

## 10. Decisions (formerly open questions)

| # | Question | Decision | Source |
|---|---|---|---|
| D1 | Window model | Single window; sidebar switches workspace; per-worktree tab sets | Design + Omar |
| D2 | Agent surface | v1: pi TUI in embedded terminal; Phase 2: custom RPC chat tab ("Agent chat — soon") | Design + Omar |
| D3 | Git UX in harness | Yes: diff review panel, PR/CI badges, ahead/behind, dirty state; PR data via `gh` CLI | Design + Omar |
| D4 | Multi-repo | Day one — sidebar groups by repo | Design |
| D5 | Tab scope | Per-worktree tab sets | Omar |
| D6 | Archive vs Delete | Archive keeps branch + PR ref + session JSONL (training data); Delete destroys all, behind confirm | Omar |
| D7 | Theming | Dark/light + configurable accent, IBM Plex Sans/Mono, tokens per design file | Design |
| D8 | `main` entry behavior | Read-only workspace on the main checkout: editor/terminal tabs, no agent spawned, no diff panel content of its own | Omar |
| D9 | Approval policy | User-selectable setting (per repo, worktree-overridable). v1 default = pi's stock approval behavior; the setting maps onto pi config/extension gating. Revisit defaults when sessions run unattended | Omar |
| D10 | Diff base | Merge-base with origin's default branch (`git merge-base origin/<default> HEAD`), so the panel always shows "what this task changes" | Omar |

All architecture-level questions are now closed; anything further is implementation detail owned by the spikes.

---

*Sources: pi repo docs (`packages/coding-agent/docs/{rpc,models,custom-provider,extensions,sdk}.md`, MIT license), zed-industries/zed crate manifests (license fields verified per-crate), LM Studio developer docs (OpenAI-compat endpoints, tool use), Claude Design project "Coding Agent Harness UI" — `Harness.dc.html`.*
