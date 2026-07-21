# openOrchestra — Architecture Document

**Status:** Draft v3.1 · July 21, 2026
**Scope:** A native Rust desktop harness (product name **openOrchestra**; repo `omar-harness`) for running coding agents across parallel git worktrees, with embedded Zed-grade editing, navigation, and terminals.
**v3 changes:** All four §9 spikes are **complete** and a post-spike MVP integration pass ("openOrchestra") has landed — this doc is rewritten to record what the spikes *proved* rather than what they *planned*. Key updates: R1 (Zed `editor` cone) and R6 (theme mapping) **retired/resolved**; the pi package is **`@mariozechner/pi-coding-agent`** (not `@mariozechner/pi`); the terminal now uses Zed's real **`terminal_view`** (the Spike 1 custom view was interim); the product is renamed **openOrchestra**; §9 recast as a completed record; new §11 (current built state) and §12 (MVP → 1.0 roadmap). Phase 2 (RPC chat) and the `agent-schema`/`training` stubs remain the principal unbuilt pieces.
**v3.1 additions (design, not yet built):** two new directions folded in. (1) **Project navigation** — adopt Zed's `Workspace` entity to get a real file explorer (`project_panel`), quick-open **⌘P** (`file_finder`), and project-wide search **⌘⇧F** (`search`), opening real worktree files; this brings `Project::local` in and absorbs the LSP work (new §3.6, reverses Spike 3's "render without a Project" for the navigation direction). (2) **Agent-agnostic runtime** — the substrate is running any agent's own CLI (**pi, OpenCode, Codex, Claude Code, Kimi**) in the embedded terminal, like pi today; an `AgentRuntime` adapter layer adds an *optional* status/notification enhancement where the agent exposes a signal, feeding a unified bus and **native OS notifications when an agent needs input** (new §4.5–4.6). pi remains the *reference* runtime that closes the training loop. New risks R10–R12, decisions D12–D14, roadmap items incl. a proving Spike 5.

---

## 1. Purpose and goals

We are building an agentic development harness with three defining properties:

1. **Agent-agnostic mission control, with a first-party loop.** The harness runs coding agents across worktrees regardless of vendor — **pi, OpenCode, Codex, Claude Code, and Kimi are all first-class** (§4.5). Each runs as its own user-installed CLI in an embedded terminal (like pi today), and — *where the agent exposes a signal* — is additionally observed and notified through one normalized model. **pi-coding-agent is the *reference* runtime**: it is the one we train ourselves and can hack end-to-end (tool schema, system prompt, conversation format), so for pi the harness conventions and the training data are the same thing (§4.4, §6.4). The other agents are supported peers — we drive and observe them, but do not train them. *(v3.1 reframes the original "our model, our loop" goal: the loop is still first-party via pi; the harness around it is now agent-agnostic.)*
2. **Worktree-native parallelism.** The unit of work is a git worktree. Each worktree gets its own agent session, editor, and terminal. Running N tasks means N isolated worktrees, visible side by side — across multiple repos from day one.
3. **Native UI, real editor + navigation.** The shell is a Rust app built on GPUI (Zed's UI framework), embedding Zed's editor, terminal, **and navigation** crates directly — actual panes, a real file tree, quick-open, and project search inside our app, not orchestrated external windows.

Non-goals for v1: collaboration/multiplayer, remote development, Windows support (macOS first, Linux second), plugin ecosystem, non-GitHub forges. (Custom agent chat UI is Phase 2 — §4.1 — but the multi-agent *runtime + notification* layer of §4.5–4.6 is in scope for the near-term roadmap.)

**Naming.** The repository and Cargo workspace are `omar-harness`; the shipped macOS application and user-facing product are **openOrchestra** (`openOrchestra.app`, state in `~/.openorchestra`). "The harness" and "openOrchestra" refer to the same thing throughout.

## 2. System overview

```
┌────────────────────────────────────────────────────────────────────┐
│  openOrchestra app (Rust, GPUI) — single window                    │
│  ┌──────────────┬──────────────────────────────┬─────────────────┐ │
│  │ WORKTREES    │  Tab strip (per worktree)    │ DIFF panel      │ │
│  │ sidebar      │  [❯_ feat/wt-nav][{} main.rs]│ changed files   │ │
│  │  repo A      │ ┌──────────────────────────┐ │ +38 −8          │ │
│  │   ◦ main     │ │ Terminal tab:            │ │ per-file diff   │ │
│  │   ◦ wt: nav  │ │  ── pi TUI runs here ──  │ │ view            │ │
│  │     [RUNNING]│ │  (v1 agent chat surface) │ │                 │ │
│  │   ◦ wt: diff │ ├──────────────────────────┤ │                 │ │
│  │     [WAITING]│ │ Editor tab (Zed editor   │ │                 │ │
│  │  repo B      │ │  crate — syntax; LSP TBD)│ │                 │ │
│  │   ◦ …        │ └──────────────────────────┘ │                 │ │
│  │ [+ new]      │  status bar: agent · branch  │                 │ │
│  └──────────────┴──────────────────────────────┴─────────────────┘ │
│                                                                    │
│  Worktree manager ── Forge poller (gh) ── Status watcher (live)    │
│                       Agent supervisor ── RPC client (Phase 2)     │
└──────────┬───────────────────┬─────────────────────────────────────┘
           │                   │
     git worktree        ┌─────┴──────────────┐  v1: pi TUI in embedded pty
     add/remove          │ pi-coding-agent    │  P2: also `pi --mode rpc`
     gh pr/checks        │ (Node.js) per wt   │       headless sidecars
                         └─────┬──────────────┘
                               │ OpenAI-compatible HTTP
                         ┌─────┴───────────────┐
                         │ LM Studio (local)   │ ← our trained model
                         │ localhost:1234/v1   │   + cloud baselines
                         └─────────────────────┘
```

*(v3.1: the single "pi" box generalizes to a per-worktree `AgentRuntime` adapter — pi / OpenCode / Codex / Claude Code / Kimi — each just a user-installed CLI run in the worktree terminal; those that expose a signal normalize it into the same status model + notification bus; see §4.5.)*

Three subsystems, cleanly separable:

- **Shell (Rust/GPUI):** windowing, panes, editor, terminal, **file explorer / quick-open / project search** (§3.6), sidebar, diff panel. Built as `crates/harness-app` (the openOrchestra binary), plus three UI-free backend crates (`worktrees`, `forge`, `status-watch`).
- **Agent runtime (adapter layer):** the substrate is running each agent's own CLI in the worktree terminal pty (like pi today). An `AgentRuntime` trait (§4.5) picks the command per `agent_kind` and, *optionally*, wires up that agent's signal surface. **pi-coding-agent** (TypeScript) is the reference: agent loop, tools, session persistence, provider layer, run as its TUI (Phase 2: also headless RPC sidecars). **OpenCode / Codex / Claude Code / Kimi** run the same way — user-installed CLIs — with optional enhancements via their own surfaces (HTTP/SSE, `notify`, hooks, ACP). We reimplement no agent loop.
- **Model serving (LM Studio):** OpenAI-compatible endpoint serving our trained model locally — the provider for the pi reference runtime. Swappable for vLLM/SGLang later without touching the harness. (Other agents bring their own provider config.)

## 3. The shell: GPUI + Zed crates

### 3.1 Framework choice

GPUI is Zed's GPU-accelerated UI framework, licensed **Apache-2.0** and usable standalone. Not published as a stable crates.io release; we depend on a **pinned git revision** of `zed-industries/zed` and upgrade deliberately, keeping gpui and the editor crates on the same revision.

**Pinned rev (proven, Spikes 1/3/4):** tag **`v1.11.3`**, commit `952d712dac48a4af2c54fb22c82d82a9d69b72d4`.

Two out-of-tree consumption facts discovered during the spikes, now standing upgrade discipline:

- **The gpui/gpui_platform split.** As of ~v1.11, gpui split into `gpui` + `gpui_platform` (+ per-OS `gpui_macos`/`gpui_linux`/…). The entry point is `gpui_platform::application().run(...)`, not the older `gpui::Application::new()`. Blog/example snippets predating the split are stale. (If v1.11.x shows macOS breakage, the fallback rev is `v1.10.3` / `0c54c414…`, but that predates the split and would need the old entry form.)
- **`[patch.crates-io]` does not propagate** from a git dependency's workspace to an out-of-tree consumer. The relevant entries from zed's root `Cargo.toml` (`async-process`, `async-task`, `calloop`, `notify`, `notify-types`) are replicated in `harness-app/Cargo.toml`. **Re-diff zed's root `Cargo.toml` on every rev bump** (R1 sub-risk).

`harness-app` is deliberately **excluded from the repo-root Cargo workspace** and carries its own `[workspace]` table + `Cargo.lock`, so the ~880-crate zed dependency cone stays out of the headless crates' build graph.

### 3.2 Editor panes — **R1 retired**

We reuse Zed's `editor` crate (and its dependency cone: `project`, `language`, `buffer`/`text`, `settings`, `theme`, tree-sitter grammars) for production-grade editor tabs. **Spike 3 was the go/no-go gate for this and it passed:** the entire cone (`workspace`, `project`, `lsp`, `dap`, `db`, `multi_buffer`, `client`, `node_runtime`) compiled out-of-tree at the pinned rev **with zero `[patch.crates-io]` changes** beyond Spike 1's set, in ~4 min cold. The fallback custom-editor plan is no longer needed.

Two design choices that keep the editor light in v1:

- **Renders without a `Project`.** `Editor::for_buffer(buffer, None, window, cx)` gives a real embedded editor pane with no `workspace`-crate runtime and no LSP. `editor::init(cx)` is called (safe globals) but not required.
- **One grammar, not eighteen.** We avoid the heavy `languages` crate (all grammars + LSP adapters + `pet*`). Instead the light `grammars` crate (rust-embed of zed's `config.toml` + `.scm` queries, *without* its `load-grammars` feature) + `tree-sitter-rust` directly, so exactly **one** grammar compiles. Parsing runs on the gpui `BackgroundExecutor`.

| Crate | License | Role |
|---|---|---|
| `gpui` / `gpui_platform` | Apache-2.0 | UI framework — safe to build on |
| `editor` | GPL-3.0-or-later | Editor pane |
| `project` | GPL-3.0-or-later | Worktree/file/LSP model (needed for LSP — Phase D) |
| `language` | GPL-3.0-or-later | Syntax, tree-sitter, language config |
| `text` | GPL-3.0-or-later | Rope/buffer primitives |
| `terminal`, `terminal_view` | GPL-3.0-or-later | Zed's terminal wrapper over alacritty |
| `grammars` (zed) | GPL-3.0-or-later | Embedded grammar config + queries (Rust only, in v1) |
| `alacritty_terminal` (zed fork) | Apache-2.0 | Terminal emulation core |

**Licensing consequence (R2 now actual):** linking `editor`/`terminal`/`grammars` makes the openOrchestra binary **GPL-3.0-or-later**, recorded in `harness-app/Cargo.toml`'s `license` field. Decision unchanged from v2: acceptable (internal tool; open-sourcing under GPL is an option, selling proprietary licenses is not).

**LSP / rust-analyzer is deferred (Phase D), not a build risk.** The full `project`+`lsp`+`client`+`node_runtime` cone already compiles under `editor`, so wiring rust-analyzer is *construction*. Verified minimal path: `Project::local(client, node, user_store, languages, fs, None, flags, cx)` with `Editor::for_buffer(buf, Some(project), …)`; the editor auto-registers the buffer with `LspStore`. **Gotcha found in Spike 3:** at this rev `LocalProjectFlags::default()` sets `init_worktree_trust: true`, which stalls LSP on a headless trust prompt — you MUST pass `LocalProjectFlags { init_worktree_trust: false, .. }`. This path needs the heavier `languages` crate + `languages::init(...)`. Sequenced in §12.

### 3.3 Terminal panes — now real `terminal_view`

v1 uses Zed's `terminal` + **`terminal_view`** crates (GPL alongside the editor, GPUI-native, consistent rendering). **In v1 the terminal is also the agent chat surface** (§4.1): each worktree's primary terminal tab runs pi's own TUI. Additional plain-shell terminals via ⌘T.

Evolution across the spikes: Spike 1 shipped a ~150-line **custom** terminal renderer over the `terminal` model crate, because the full `terminal_view` renderer pulls `workspace` (roughly half of zed). Spike 3 brought `workspace` into the graph anyway (via `editor`), and the post-spike MVP therefore **swapped the custom view for zed's real `terminal_view::TerminalView`** — gaining selections, scrollback, hyperlink hover, per-cell backgrounds, and IME "for free." `TerminalView::new` needs `WeakEntity<Workspace>` + `WeakEntity<Project>` handles, which the app now constructs. The Spike 1 custom-view limitations (no scrollback, no per-cell bg, no IME) are therefore **resolved**.

### 3.4 UI spec (from `docs/harness-ui/Harness.dc.html`)

The design is the source of truth for v1 chrome; key commitments:

- **Single window, three columns.** Left: worktree sidebar (resizable, collapsible). Center: tab strip + content + status bar. Right: diff panel (resizable, collapsible). Top bar: panel toggles, `openOrchestra · <active-branch>` breadcrumb, theme toggle.
- **Worktree sidebar.** Grouped by repo (multi-repo day one), each group: `main` entry (default branch, CI check) + worktree cards. Card anatomy: editable avatar (2-letter initials + color from 8-swatch palette), inline-renamable name, mono branch label, PR badge (`#N` — open/draft/merged/closed colors), ahead/behind counts, dirty indicator (`~N` files), CI status (pass/fail/running), agent status pill (**Running** pulse / **Waiting** amber / **Idle** gray), hover/right-click actions: archive, delete.
- **Tabs are scoped per worktree** — each worktree remembers its own tab set (terminals + editors); switching worktree cards swaps the strip. Tab menu: New terminal ⌘T, New editor ⌘E, Agent chat (Phase 2).
- **Diff panel.** Collapsible changed-files list (per-file status M/A/D badge, +adds/−dels, totals) above a scrollable unified diff view with sticky file header. Driven by git status/diff against the worktree's merge base.
- **Status bar.** Agent state (mirrors the pill), branch, encoding, LSP server indicator (LSP indicator is a placeholder until Phase D).
- **Theming.** Dark + light palettes with a configurable accent color; fonts IBM Plex Sans / IBM Plex Mono (the MVP ships Menlo for the mono/terminal slot pending Plex bundling). Design tokens (`--bg0..2`, `--surface*`, `--tx*`, `--accent`, semantic green/red/amber/purple, diff add/del tints) are the `HarnessTheme` struct; Zed editor/terminal crate themes are **generated from the same tokens** so panes match the chrome (see §3.5, R6 resolved).

### 3.5 Theme generation (R6 resolved)

`src/harness_zed_theme.rs` builds a `ThemeFamilyContent` from the `HarnessTheme` tokens via a `json!` value (every content field is `Option`/`#[serde(default)]`, so a partial object deserializes and `theme_settings::refine_theme_family` fills the ~100 other fields with coherent dark defaults), registers it in the `ThemeRegistry`, and activates it with `GlobalTheme::update_theme`. Because both `editor` and `terminal_view` read `cx.theme()`, **one token set now drives chrome + editor + terminal + syntax** — the R6 goal, unit-tested (`tokens_map_into_resolved_theme`: tokens survive into `editor_background`, `terminal_ansi_*`, and syntax captures). Note: the crate root needs `recursion_limit = "256"` because the theme `json!` literal nests past the default 128. Follow-up: syntax hues outside the chrome token set (function purple, type teal, number orange) are currently literals — a first-class `HarnessTheme` syntax ramp is the tidy extension.

### 3.6 Project navigation: file explorer, ⌘P, ⌘⇧F (planned — Workspace pivot)

**Requirement (Omar):** each worktree's editor gets a real **file explorer** that opens actual files of the project/worktree, and these keybindings work: **⌘⇧F = search inside all files** (project-wide content search), **⌘P = open file** (fuzzy quick-open).

**Decision (D13): adopt Zed's `Workspace` entity per worktree** rather than hand-building these. Zed already ships exactly these features as crates that plug into a `Workspace`:

| Feature | Zed crate | Default action / key | Notes |
|---|---|---|---|
| File explorer (tree) | `project_panel` | dockable `Panel`; reveal `cmd-shift-e` | Left dock inside the Workspace; opens files as editor items |
| **Quick-open (⌘P)** | `file_finder` | `file_finder::Toggle` → `cmd-p` | Modal registered on the Workspace; fuzzy over the Project's worktree |
| **Project search (⌘⇧F)** | `search` | project search deploy → `cmd-shift-f` | Ripgrep-backed, results pane in the Workspace |
| Editor / terminal panes | `editor`, `terminal_view` | tabbed `Item`s in a `Pane` | `terminal_view` is *already* a Workspace item — this unifies it |

**Why this is the right pivot (and what it reverses).** Spike 3 deliberately rendered the editor *without* a `Project` (`Editor::for_buffer(buffer, None, …)`) to stay light. `project_panel`/`file_finder`/`search` are all built around a real zed **`Workspace`** backed by a **`Project::local(...)`** whose worktree roots are the worktree directory. Bringing `Workspace` + `Project::local` in is therefore the same step §12/M3 already required for **LSP** — so navigation and rust-analyzer land together off one pivot, and we drop the bespoke Weak-handle plumbing the MVP hand-rolled for `terminal_view`. Net: v3.1 **supersedes the "render without a Project" stance** of §3.2 for worktrees that want full IDE affordances.

**Integration shape.** Per selected worktree, the center region hosts a zed `Workspace` (its own left `project_panel` dock, tabbed editor/terminal items, and the `file_finder`/`search` modal layer). openOrchestra's **outer chrome stays custom**: the worktree sidebar (left) and diff panel (right) remain the harness's own views wrapping the Workspace, not zed panels. Keybindings come from **loading zed's default keymap** (or a curated subset) so `cmd-p`, `cmd-shift-f`, `cmd-shift-e`, tab nav, etc. resolve to the crate actions above.

**R10 RESOLVED — Spike 5 passed (July 21, 2026).** `Workspace` does *not* in fact insist on owning the window: `Workspace::new` takes a `&mut Window` but is an ordinary `Render` entity, so it drops into our center column as a child view like any other. It is hosted behind a third per-worktree tab (**▤ files**), built lazily on first open because `Project::local` starts a full worktree scan and paying that on every pane creation would tax every worktree switch to benefit only the visits that use the tree.

Four concrete findings, each of which the spike existed to pin down:

1. **A minimal real `AppState` is buildable out of tree.** zed's `main.rs` assembles one across ~200 lines tangled with telemetry, installation IDs, and auto-update; none of that is load-bearing. The struct has eight fields and each has a directly callable constructor. `harness-app/src/workspace_host.rs` is the whole thing.
2. **The keymap names are not the Rust paths** — the single most likely thing to have been guessed wrong: `"file_finder::Toggle"` → `workspace::ToggleFileFinder`; `"pane::DeploySearch"` → `workspace::pane::DeploySearch`; `"project_panel::ToggleFocus"` → **`zed_actions::project_panel::ToggleFocus`** (the panel crate's own `ToggleFocus` is private). Bound in the `"Workspace"` key context so they don't fire over our sidebar.
3. **Spike 3's `init_worktree_trust: false` gotcha is real and still required.** The default `true` stalls on a trust prompt nothing in our chrome can answer.
4. **`AppState.session` is read for exactly one thing** — `session.id()`, a string registered with the `WorkspaceStore`. Since openOrchestra keeps its own state (§6.1) and never restores zed sessions, a fresh history-less session is correct, avoiding a dependency on zed's sqlite session database purely to discard the result.

Also worth recording: the harness passes a **blocked HTTP client**, so zed's collab and telemetry paths stay inert rather than reaching out from under us, and `node_runtime` is `unavailable()` since agents resolve node from `$PATH` themselves (§4.5/R12) and zed's managed runtime would download its own.

## 4. Agent runtime: multi-agent, one per worktree

**Package identity (corrected in v3).** The agent is **`@mariozechner/pi-coding-agent`** (binary `pi`), from `badlogic/pi-mono`, `packages/coding-agent`. The similarly-named `@mariozechner/pi` on npm is a **different tool** (a vLLM "pods" deployment CLI, binary `pi-pods`); every v2 reference to "pi" means `pi-coding-agent`. **Supported pin: `0.73.1`** — reconciled in M2 and consistent across setup, doctor, `pi-ext/package.json`, and the extension header. **Rename incoming:** the project has moved to `earendil-works/pi-mono` and the npm scope is being renamed to **`@earendil-works/pi-coding-agent`** (0.73.1 ships self-update support for exactly that migration); update all four pin sites when it publishes.

### 4.1 Phased interaction model

**Phase 1 (v1): pi TUI in the embedded terminal.** Each worktree's primary terminal tab runs `pi` interactively — chat, steering, approvals, /commands all come from pi's mature TUI at zero UI cost. The harness's v1 responsibilities: spawn pi in a pty with `cwd` = worktree root, keep session files organized per worktree, and surface agent state in the sidebar.

**Agent status without RPC (built, Spike 2 + wired in MVP):** a pi **extension** (`pi-ext/harness-status.ts`, loaded per project) subscribes to agent lifecycle events and writes `{state, timestamp, sessionFile}` to `.harness/status.json` in the worktree. The Rust **status watcher** (`crates/status-watch`) file-watches these to drive the sidebar pills and status bar. The MVP connects this pipeline to the live sidebar. Same mechanism can later append outcome metadata for training-data harvesting.

**Reality vs. the v2 sketch (measured against real pi 0.70.6, Spike 2):**

| v2 sketch | Reality | State written |
|---|---|---|
| `agent_start` | 1:1, exists | `running` |
| "UI dialog open" event | **No such event.** All extensions share one `ExtensionUIContext` per runner; the status extension wraps its `select`/`confirm`/`input`/`editor` methods, so any extension-opened dialog (the approval path, D9) flips state while the promise is pending. pi-core TUI dialogs (model picker) are invisible — acceptable (user-initiated, not "agent blocked"). | `waiting` |
| `agent_settled` | **RPC-only; not an extension event.** Approximated as `agent_end` + `!ctx.hasPendingMessages()` + a debounced write (`HARNESS_STATUS_SETTLE_MS`, default 750 ms) cancelled by a following `agent_start`. Auto-retry re-fires `agent_end`/`agent_start` per attempt with no `willRetry` flag; the debounce absorbs fast retries, but long provider-backoff gaps still blip idle (R8). | `idle` |
| — | `session_start` / `session_shutdown` | initial/forced `idle` + fresh `sessionFile` |

**Phase 2: custom agent chat panel over RPC.** When "Agent chat" graduates from "soon," the agent supervisor additionally spawns `pi --mode rpc` sidecars and the chat tab renders the event stream natively. §4.2–4.3 is the contract for that phase — specified now so Phase 1 decisions (session layout, extension architecture, status schema) stay compatible. **Not yet built** (the `pi-client` crate does not exist); see §12.

### 4.2 IPC contract (Phase 2)

pi's RPC mode is JSONL over stdin/stdout with strict framing: **split records on `\n` only** (accept a trailing `\r`; do not treat U+2028/U+2029 as newlines). Commands may carry an `id` for correlation; responses echo it. Events carry no `id`.

**Corrected against pi 0.73.1 (M8) — two items in the v3 sketch did not exist.**

Commands (verified from `dist/modes/rpc/rpc-types.d.ts`): `prompt` (with `streamingBehavior: "steer" | "followUp"`), `steer`, `follow_up`, `abort`, `new_session`, `switch_session`, `fork`, `clone`, `get_state`, `get_messages`, `get_fork_messages`, `get_last_assistant_text`, `get_session_stats`, `get_commands`, `set_model`, `cycle_model`, `get_available_models`, `set_thinking_level`, `cycle_thinking_level`, `set_steering_mode`, `set_follow_up_mode`, `compact`, `set_auto_compaction`, `set_auto_retry`, `abort_retry`, `bash`, `abort_bash`, `export_html`, `set_session_name`.

> ⚠️ **`get_tree` and `get_entries` do not exist.** v3 called `get_entries` (with a `since` cursor) "the durable resume primitive across harness restarts". There is no such command and no cursor. Resume means replaying the session file (§6.4's format), not a cursor query.

Event stream: `agent_start` → `turn_start` → `message_start` → `message_update` → `tool_execution_start/update/end` → `turn_end` → `agent_end`. Plus `queue_update`, `compaction_start/end`, `auto_retry_start/end`, `extension_error`.

> ⚠️ **`agent_settled` does not exist**, and `agent_end` carries `messages`, not `willRetry`. v3 called `agent_settled` "the true done signal" and R8 expected it to retire the Phase 1 debounce. What actually retires R8 is **`auto_retry_start`/`auto_retry_end`**: Phase 1 had to *guess* at retries with a 750 ms debounce because `agent_end`/`agent_start` re-fire with no flag, whereas RPC states a retry outright — so a long provider backoff can no longer blip idle at all. Implemented as `SettleTracker` in `crates/pi-client`.
>
> Also: `message_update`'s payload field is **`assistantMessageEvent`**, not `delta`.

Rust-side (**built — `crates/pi-client`**): serde types mirroring `rpc-types.ts`; strict LF-only framing (pi's docs explicitly warn that generic line readers splitting on `U+2028`/`U+2029` corrupt records, since those are legal inside JSON strings); a reader thread per process fanning records out to subscribers; `SettleTracker` deriving Running/Waiting/Idle. Conformance tests replay verbatim recorded JSONL from a real pi 0.73.1 run, plus an `--ignored` live test that drives an actual `pi --mode rpc` against a local stub provider. Unknown record types degrade to `RpcRecord::Unknown` rather than killing the session. Pin the pi version; treat protocol upgrades like schema migrations.

### 4.3 Extension-UI sub-protocol (Phase 2)

Dialog methods (`select`, `confirm`, `input`, `editor`) arrive as `extension_ui_request` and block the agent until a matching-`id` `extension_ui_response` (or agent-side timeout) — these become native modals. Fire-and-forget methods (`notify`, `setStatus`, `setWidget`, `setTitle`) map to toasts/status-bar entries. In Phase 1, pi's own TUI handles all of this; the "Waiting" pill is driven by the status extension's shared-`uiContext` wrap.

### 4.4 Customizing the agent for our model

Why pi: the loop is minimal and everything we need to co-evolve with training is pluggable without forking, via TypeScript extensions loaded per-project or per-user. **Confirmed registration surface (Spike 2, v0.70.6):** an extension is a TS module whose default export is a factory `(pi: ExtensionAPI) => void | Promise<void>`, subscribing via `pi.on(eventName, handler)`; handlers get `(event, ctx: ExtensionContext)` with `ctx.cwd`, `ctx.ui`, `ctx.sessionManager.getSessionFile()`, `ctx.isIdle()`, `ctx.hasPendingMessages()`, `ctx.abort()`. Loaded via jiti (no compile step) from `~/.pi/agent/extensions/*.ts`, `.pi/extensions/*.ts`, `pi -e <path>`, or `settings.json`. The harness symlinks/copies `harness-status.ts` into each worktree's `.pi/extensions/` when spawning the pty.

- `pi.registerTool()` / `pi.setActiveTools()` — make the runtime toolset exactly match our training tool schema (names, arg shapes, result formats). Harness-specific tools (worktree-aware ops, "request review") are extensions too, as is the status extension (§4.1).
- System prompt and prompt templates are overridable; the SDK (`createAgentSession`) is the escape hatch if we outgrow the stock CLI entry point. pi is MIT — forking is a supported failure mode, not a crisis.

**Contract discipline:** the tool schema is versioned in this repo (`agent-schema/`) as the single source of truth; the training-data generator (`training/`) and the pi extension both import from it. Model checkpoints declare their schema version; the harness refuses (or adapts) on mismatch. **Both directories are currently empty stubs** — see §12. *(This contract applies to the pi reference runtime only; peer agents in §4.5 bring their own tool schemas.)*

### 4.5 Multi-agent runtime: agents are CLIs in a terminal (design)

**The substrate is the embedded terminal.** Running an agent means what it already means for pi: **spawn the agent's own CLI in the worktree's terminal pty** (`cwd` = worktree root) and let the user drive its TUI. `claude`, `codex`, `opencode`, `kimi`, and `pi` are all just user-installed, user-authenticated command-line tools — launching one in a terminal we opened is identical to the user typing it in Terminal.app. The harness **bundles, redistributes, and links none of them** (they are separate subprocesses resolved from `$PATH`, exactly like pi today), so there is **no license coupling and no ToS concern** (R12). This baseline already works for any such CLI (Spike 1's pty path) and is all an agent strictly needs to be usable here.

**The adapter adds *optional* enhancements on top.** A worktree records **which agent it runs** (`agent_kind`, §6.1); an `AgentRuntime` adapter per vendor (a) knows the command to spawn plus any per-agent config to drop in, and (b) *where the agent exposes a signal surface*, translates that agent's native "needs input" event into the **same normalized state the sidebar already consumes** — `Running`/`Waiting`/`Idle` (the frozen `.harness/status.json` states, Spike 2 §3) — plus a harvestable session path when one is available. **An agent with no signal surface still runs fully in its terminal**; it just shows a coarser status and raises no OS "needs input" ping.

```rust
enum AgentKind { Pi, OpenCode, Codex, ClaudeCode, Kimi }
enum AgentState { Running, Waiting, Idle, Unknown }       // == status.json states
struct AgentSignal { state: AgentState, reason: WaitReason, session_file: Option<PathBuf> }
enum WaitReason  { PermissionPrompt, IdlePrompt, TurnComplete, None }
enum SignalKind  { StatusFile, Sse, Notify, Acp, None }   // how the enhancement arrives (if any)

trait AgentRuntime {
    fn kind(&self) -> AgentKind;
    fn command(&self) -> Command;                         // the CLI to run in the pty (from $PATH)
    fn on_spawn(&self, wt: &Worktree) -> Result<()>;      // optional per-agent config injection
    fn subscribe(&self) -> Option<Receiver<AgentSignal>>; // None ⇒ terminal-only, still fine
    fn fidelity(&self) -> WaitFidelity;                   // Exact | BestEffort | TerminalOnly
}
```

**Per-agent surfaces (v3.1 research — every agent runs in the terminal; the signal column is the *optional* enhancement):**

| Agent | Command | License | Optional "needs input" signal → normalization | What the adapter drops in |
|---|---|---|---|---|
| **pi** (reference) | `pi` | MIT | `harness-status.ts` → `.harness/status.json` `waiting` (shared-`uiContext` wrap); P2: RPC `extension_ui_request` | copy `harness-status.ts` into `.pi/extensions/` |
| **OpenCode** | `opencode` | MIT | subscribe **`GET /event`** SSE on its local server: `permission.updated`→`Waiting`, `session.idle`→`Idle`; reply `POST /session/:id/permissions/:id` | nothing (server runs alongside the TUI) |
| **Claude Code** | `claude` | proprietary¹ | **`Notification` hook** (matchers `permission_prompt`/`idle_prompt`, + `Stop`→`Idle`) → shim writes `.harness/status.json`; payload carries `transcript_path` (harvestable) | project-local `.claude/settings.json` hook → shim |
| **Codex** | `codex` | Apache-2.0 | `notify` program gets **`agent-turn-complete`** only (turn done); **no out-of-band approval-blocked event** in TUI mode (R11) | `notify = ["<shim>"]` in `config.toml` |
| **Kimi Code** | `kimi` | Apache-2.0 | pure TUI exposes no hook; **`kimi acp`** speaks Agent Client Protocol (the JSON-RPC protocol Zed uses for external agents, incl. permission requests) — a structured path when the TUI isn't required | nothing for the TUI; ACP is an alternate run mode |

¹ Proprietary is not a problem here: we spawn the user's own installed `claude`, we don't ship it (R12).

Design consequences:

- **The status file is the shared normalization target for the file/hook adapters** (pi, Claude Code) — both write the same 3-state `.harness/status.json` via tiny shims, so the existing `status-watch` crate consumes them unchanged. OpenCode (SSE), Codex (`notify`), and Kimi-via-ACP translate in-process and emit `AgentSignal` on the bus instead. The sidebar pill is agent-neutral either way.
- **Signal fidelity is heterogeneous and shown honestly** (R11). *Exact:* pi (P2), OpenCode, Claude Code. *Best-effort:* Codex (turn-complete only; no approval signal in TUI). *Terminal-only until wired:* Kimi's TUI (ACP is the upgrade path). The UI reflects `fidelity()` rather than pretending parity — and a terminal-only agent is a supported first-class case, not a failure.
- **Harvesting is per-adapter** (§6.4): pi exposes session JSONL and Claude Code exposes `transcript_path`; Codex/OpenCode/Kimi keep their own stores. The training loop stays pi-first (§4.4); other agents are harvestable only where their adapter surfaces a session path.

### 4.6 Notification bus + native OS notifications (design)

**Requirement (Omar):** "notify via notification when input is needed." Today the `waiting` state only drives the in-app sidebar pill. v3.1 adds a **notification bus** and a **native OS notification** emitter.

- **Bus.** The existing fan-out pattern (forge poller + status watcher already deliver on channels) generalizes: every adapter's `AgentSignal` stream, plus forge/git updates, feed one in-app bus that GPUI entities (sidebar, status bar, and the new notifier) subscribe to. No new IPC — it is the same channel discipline the backend crates already use.
- **OS notifications.** On any worktree transition **into `Waiting`** (permission/idle prompt), the notifier posts a native notification — title = worktree name + repo, body = the `WaitReason`, click = focus that worktree/tab. It **suppresses** when the app is already focused on that worktree (no self-spam), coalesces rapid flaps (reuse the 750 ms settle debounce, Spike 2), and can be scoped per-repo/worktree by the D9 approval setting. Because openOrchestra now ships as a bundled **`.app`** (MVP packaging), native macOS notifications via `UNUserNotificationCenter` are available — a bundle identity is the prerequisite macOS imposes, and we already have one. A cross-platform crate (`notify-rust`) is the Linux path.
- **Coverage is gated on the adapter's signal (R11).** Only agents whose adapter emits a `Waiting` signal (§4.5) raise a "needs input" notification: pi (P2), OpenCode, Claude Code fire exactly; Codex fires only on *turn-complete* (labelled as such, not "input needed"); a terminal-only agent (Kimi TUI, until ACP) raises none and simply runs in its terminal. The notifier honors `fidelity()` so we never raise a false "input needed."

This lives behind a small `notifier` module in `harness-app` (UI-adjacent) fed by the bus; the adapters and bus are UI-free and testable like the other backend crates.

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

Keyless local servers still need a dummy `apiKey`. **The block above is verified as written (M2):** real pi 0.73.1 loads exactly this JSON, resolves `lmstudio/omar-agent-v1`, and streams an OpenAI-compatible response through it (`pi-ext/testing/`). What remains unconfirmed is narrower than before — the `compat` flags describe *LM Studio's* behavior, so they still want one check against a live LM Studio build rather than the stub. Frontier models stay configured alongside as baselines — free A/B via pi's `/model`, same harness, same tools. Migration to vLLM/SGLang is a `baseUrl` change.

## 6. Worktree orchestration — **built (Spike 4 + MVP)**

Implemented as the UI-free `crates/worktrees` crate (in the root workspace, real-git tested) plus app integration in `harness-app`.

### 6.1 Lifecycle

- **Creation:** `git worktree add <state-dir>/worktrees/<repo>/<task-slug> -b <branch>` from a chosen base ref. One task = one worktree = one branch = one pi session. On open, the primary terminal tab spawns pi. The MVP captures a **name** via an inline text input; the branch is derived (`feat/<slug>`) and the base is the compare ref (branch/base *selection* is a §12 follow-up).
- **Archive** (D6): remove the worktree directory, **keep** the branch, PR ref, and session JSONL. Default end-of-task action — sessions are training data (§6.4).
- **Delete** (D6): destroy worktree, local branch, and session, behind a **two-click confirm**. For abandoned experiments only.
- **Registry** (`registry.rs`): on-disk JSON index (atomic write-temp + rename) mapping worktree → repo, root, branch, slug, editable name, path, session file, avatar (initials + palette color), PR number, created-at, archived, **and `agent_kind`** (pi / opencode / codex / claude-code — additive field, defaulting to `pi` for forward compatibility; §4.5 dispatches spawn on it). Renames and avatar edits mutate the registry, never git state. `created_at` is caller-supplied so the crate has no clock (deterministic tests). Manager ordering is **git op first, then registry persist**, so a git failure never leaves a phantom entry.
- **State location:** harness state lives under `~/.openorchestra/` (`registry.json` + `worktrees/`) so worktrees never nest inside the repo. (Spike 4 used `<repo_parent>/.omar-harness/`; the MVP moved it to the home-dir state root.)

### 6.2 Status

Sidebar card state is derived from: the status watcher (§4.1: running/waiting/idle), git (`GitWorktrees::status` — porcelain dirty count + `rev-list --left-right` ahead/behind), and the forge poller (§6.3: PR state, CI). All three are wired live in the MVP.

### 6.3 Forge integration — built

PR badges and CI status come from **GitHub via the `gh` CLI**: `crates/forge`, a `Forge` trait behind a `Poller`. `pr_for_branch` / `checks_for_branch` return `Result<Option<…>>` / `Result<Vec<…>>` (distinguishing "no PR" from "gh failed" for backoff). `GhCli` shells through a `CommandRunner` trait (real `SystemRunner`; tests inject a playback fake). Commands: `gh pr view <branch> --json number,state,isDraft,title` and `gh pr checks <branch> --json name,state,bucket` (bucket → Pass/Fail/Running/Skipped; unknown bucket falls back to raw state). `available()` probes `gh --version`; when false the poller exits and badges hide (R7). `Poller::spawn` runs a background thread with per-branch `interval` (default 45 s) + jitter (0–10 s, xorshift, no `rand` dep) + exponential backoff (15 s → ×2 → cap 300 s), change-driven delivery (identical results/errors suppressed), runtime `add_branch`/`remove_branch`, and skips the checks call when a branch has no PR. **gh positionals are terminated with `--`** (security pass) so branch names can't parse as flags.

### 6.4 Session exhaust as training data

Every session is an append-only JSONL tree we control the reading of. Archived sessions + final diffs + outcome labels (merged/abandoned — derivable from the forge poller) are the raw material for the next training round. The registry records enough metadata to make harvesting trivial. The `sessionFile` field in `status.json` is the harvesting hook. This closes the loop that motivates the whole project. **Harvesting itself is unbuilt** (`training/` stub — §12).

## 7. Repository and crate layout

```
omar-harness/
├── Cargo.toml            # root workspace (headless crates only)
├── crates/
│   ├── harness-app/      # openOrchestra GPUI app — EXCLUDED from root workspace
│   │                     #   (own [workspace]+lock; keeps zed cone out of the others)
│   ├── worktrees/        # git worktree manager + registry (+ agent_kind) — BUILT
│   ├── forge/            # gh-CLI poller behind a Forge trait (no UI deps) — BUILT
│   ├── status-watch/     # .harness/status.json file watcher (no UI deps) — BUILT
│   ├── agent-runtime/    # AgentRuntime adapters + notification bus (§4.5) — BUILT (A1)
│   ├── agent-schema/     # embeds + version-checks the tool contract — BUILT (M6)
│   ├── training/         # session harvesting + the `harvest` binary — BUILT (M7)
│   └── pi-client/        # Phase 2: RPC protocol types + framed stdio client — BUILT (M8)
├── pi-ext/               # TypeScript: harness-status + harness-tools extensions,
│                         #   testing/ (stub provider + capture-lifecycle.sh) — BUILT
├── agent-schema/         # v1/tools.json — the tool contract (source of truth) — BUILT
├── training/             # harvesting docs + record format — BUILT
├── script/               # doctor.sh, setup-macos.sh, package-mac.sh, gen-icon.py
├── assets/               # openOrchestra icon (.icns / .png)
├── .github/workflows/    # security-audit.yml (weekly + manifest-triggered cargo-audit)
└── docs/                 # this doc; spike2-notes.md; harness-ui/ design reference
```

Non-UI crates are deliberately headless so the same backend can drive batch evals of new checkpoints (pi RPC + registry, no window).

## 8. Risks and mitigations (v3 status)

| # | Risk | v3 status |
|---|---|---|
| R1 | Zed `editor` drags a huge dep cone, unstable APIs, no semver | **RETIRED for the editor path.** Compiled out-of-tree at the pinned rev, no patch changes, renders headless without panic (Spike 3). Fallback custom editor no longer needed. *Residual:* the `[patch.crates-io]` re-diff chore on every rev bump (below). |
| R1a | `[patch.crates-io]` replication drift on zed rev bump | **Standing chore.** Low effort, silent breakage if forgotten. Re-diff zed's root `Cargo.toml` each bump. |
| R2 | GPL-3.0 propagation to the binary | **Now actual** — linking `editor`/`terminal`/`grammars` makes the binary GPL-3.0-or-later, recorded in Cargo.toml. Accepted (§3.2). |
| R3 | pi protocol/behavior drift across versions | **Pin-gap half RETIRED (M2 done).** One pin — **`0.73.1`** — in setup, doctor, `pi-ext/package.json`, and the extension header; 0.70.6→0.73.1 diffed as purely additive for every surface the extension uses. Drift itself is inherent, but re-checking is now one command: `bash pi-ext/testing/capture-lifecycle.sh` drives a **real** pi process through a full turn against a local stub provider (no API key, no network) and asserts the `.harness/status.json` transitions; `make doctor` warns when the installed pi ≠ the pin. **New residual:** the npm scope is being renamed `@mariozechner/…` → `@earendil-works/pi-coding-agent` — update all four pin sites when it publishes. See `docs/m2-pi-pin.md`. |
| R4 | Node.js runtime dependency per worktree | Unchanged/accepted (dev tool); ptys spawn lazily; `pi` resolved from `$PATH` with `$SHELL` fallback verified. |
| R5 | Our model's tool-calling reliability behind openai-compat servers | Open (no live model yet). Schema-versioned contract (§4.4); constrained decoding in LM Studio/vLLM; frontier baseline always configured. |
| R6 | design-token → Zed theme mapping is bespoke | **RESOLVED.** One token set generates the zed theme for chrome + editor + terminal + syntax; unit-tested (§3.5). *Residual:* first-class syntax ramp in `HarnessTheme` (cosmetic). |
| R7 | `gh` CLI polling: rate limits, output drift, gh absent | **Parsing half RETIRED (M1 done).** Verified against real gh 2.95.0: field shapes, all four PR states, all four buckets, and both "absent" stderr strings confirmed; live captures committed under `crates/forge/fixtures/live/`. One documented claim was wrong — with `--json`, gh exits **0** regardless of check outcome (the 1/8 convention is human-output only); the parser was already status-independent, so no behavior change. See `docs/m1-gh-verification.md`. *Residual:* rate limits / graceful degrade — unchanged, mitigated by backoff + jitter + `available()`. *New, minor:* `gh pr view <branch>` does not resolve **fork-head** PRs, so fork-pushed worktree branches show no badge (§12 cross-cutting). |
| R8 | v1 file-watch status coarser than RPC events | **Both gaps now closed in the protocol layer (M8), though not the way v3 predicted.** (1) auto-retry blipping idle is fixed by **`auto_retry_start`/`auto_retry_end`** — not by `agent_settled`, which does not exist (§4.2). Phase 1 had to guess with a 750 ms debounce; RPC states the retry outright, so no backoff length can blip. (2) invisible dialogs are fixed by `extension_ui_request`, which reports every dialog with an id to answer on. `crates/pi-client`'s `SettleTracker` implements both, tested against a recorded session. *Residual:* the Phase 1 file path keeps its ceiling and remains the fallback until the chat panel ships. |
| R9 | Shared-`ctx.ui` wrap relies on an undocumented single-`uiContext`-per-runner detail | **Re-verified at 0.73.1 (M2).** `ExtensionRunner` still holds one `uiContext`, still exposed via a call-time getter, so the idempotent WeakSet patch behaves as before. Re-checking found a **coverage gap, now closed**: `ExtensionUIContext.custom` ("show a custom component with keyboard focus") also blocks the agent and was not wrapped, so a `custom` dialog showed **Running** while actually waiting. Now wrapped, with nested-dialog coverage. Still re-check each bump (R3 discipline) — `capture-lifecycle.sh` is that check. |
| R10 | Embedding zed's `Workspace` inside custom outer chrome (§3.6) | **RETIRED (Spike 5, July 21 2026).** `Workspace` is an ordinary `Render` entity, not a window owner, and hosts cleanly as a child of our center column — verified at runtime, with `Project::local` scanning the worktree. Action paths confirmed (and all three differ from their keymap names — §3.6). The custom-nav fallback is not needed. *Residual:* the panel/modal surfaces still need real use to shake out focus and layout edges. |
| R11 | Heterogeneous "needs input" fidelity — not every agent exposes a clean awaiting-input signal (**Codex TUI has none** — `notify` gives only `agent-turn-complete`; **Kimi TUI has none** without ACP) | **New (v3.1), low.** The terminal always works; the enhanced pill/notification is opportunistic. Adapter reports `fidelity()`; UI shows terminal-only / best-effort honestly. Exact signals available via OpenCode SSE, Claude Code hook, pi P2 RPC, `codex exec --json`/`codex mcp`, or `kimi acp`. |
| R12 | Agent licensing / ToS across vendors (Claude Code proprietary; others permissive) | **New (v3.1) — non-issue by construction.** The harness spawns the **user's own installed, authenticated CLI** from `$PATH` as a separate subprocess (like pi today); it bundles/redistributes/links none of them and runs none of their code in-process. So there is **zero license coupling** (independent of the binary's GPL status, §3.2) and no ToS concern — running `claude`/`codex`/`kimi`/`opencode`/`pi` in a terminal is ordinary first-party use. *Residual:* pin the tested version of each agent and re-check its own config/hook/event schema on upgrade (R3-style). |

## 9. Spike plan — **complete (record)**

All four §9 spikes landed on 2026-07-19; a post-spike MVP integration pass followed. Each was verified at least to `cargo check`/`build` clean + headless execution without panic (gpui falls back to a headless platform in the container); backend crates have real test suites.

1. **Spike 1 — GPUI shell + terminal + pi.** ✅ *Landed.* Pinned zed rev; three-column skeleton; terminal tab running pi's TUI in a pty (`harness-app → pi` confirmed via `pstree`). Discovered the gpui_platform split, the patch-replication chore, and the `pi-coding-agent` package identity. Proves the v1 agent path.
2. **Spike 2 — Status + forge pipeline.** ✅ *Done.* `harness-status.ts` → `.harness/status.json` → `status-watch` → live pill; `gh` poller → PR/CI badges. Mapped the extension event reality (no `agent_settled`/dialog events; approximations documented). 30 backend tests (9 watch, 21 forge). See `docs/spike2-notes.md`.
3. **Spike 3 — Zed editor pane + theme mapping.** ✅ *GO.* The §3.2 go/no-go gate — passed. Real `Editor` embedded via `for_buffer(.., None, ..)`, Rust syntax via one grammar, token→theme generation unit-tested. **R1 retired, R6 resolved.**
4. **Spike 4 — Worktree vertical slice.** ✅ *Landed.* `crates/worktrees` (registry + git + manager), real merge-base diff, per-worktree tab sets, create/archive/delete against real git. 8 real-git tests. Completes the spike plan.

**Post-spike MVP ("openOrchestra" commit):** swapped in real `terminal_view`; wired live forge badges + status pill; added right-click menu, per-repo `+`, clickable diffs, editor keymap, inline new-worktree name input, native folder picker + empty state, blinking caret, Menlo font; repo-aware state in `~/.openorchestra`; `make doctor` + macOS `.app` bundler + icon. **Security pass:** `--` arg-injection terminators (git/gh), pinned pi, weekly cargo-audit CI.

Backend workspace test count: **44 pass** (worktrees 8, forge 25, status-watch 10, +1 doc). `harness-app` builds/checks clean and runs headless without panic.

**macOS correctness fix (first real run on macOS, July 21, 2026).** The spike-era suite was only ever run in the Linux build container, which hid a genuine bug: upstream `notify` 8's **FSEvents backend reports canonicalized paths** (`/private/var/…`), while `status-watch` keyed its registry by the caller-supplied path (`/var/…`), so `starts_with` matched nothing and **no status event was ever routed** — the sidebar pill would have been frozen at its initial value on any symlinked path. inotify does not canonicalize, so Linux never saw it. `watch()`/`unwatch()` now resolve the `.harness` dir once and key on the resolved form (events are still *reported* under the caller's path), with a symlink regression test that fails on every platform rather than only macOS.

## 10. Decisions (closed)

| # | Question | Decision |
|---|---|---|
| D1 | Window model | Single window; sidebar switches workspace; per-worktree tab sets |
| D2 | Agent surface | v1: pi TUI in embedded terminal; Phase 2: custom RPC chat tab |
| D3 | Git UX in harness | Diff review panel, PR/CI badges, ahead/behind, dirty state; PR data via `gh` CLI |
| D4 | Multi-repo | Day one — sidebar groups by repo |
| D5 | Tab scope | Per-worktree tab sets |
| D6 | Archive vs Delete | Archive keeps branch + PR ref + session JSONL (training data); Delete destroys all, behind confirm |
| D7 | Theming | Dark/light + configurable accent, IBM Plex Sans/Mono, tokens per design file (generated into the zed theme, §3.5) |
| D8 | `main` entry behavior | Read-only workspace on the main checkout: editor/terminal tabs, plain shell (no agent spawned) |
| D9 | Approval policy | User-selectable setting (per repo, worktree-overridable). v1 default = pi's stock approval behavior; revisit when sessions run unattended |
| D10 | Diff base | Merge-base with origin's default branch, taken merge-base → **working tree** (shows committed *and* uncommitted task changes) |
| D11 | Product name | **openOrchestra** (repo stays `omar-harness`) — *new in v3* |
| D12 | Agent scope | **Fully agent-agnostic** — any coding-agent CLI (pi/OpenCode/Codex/Claude Code/Kimi) run in the embedded terminal behind one `AgentRuntime` adapter (§4.5); status/notify is optional per agent; pi is the reference runtime that keeps the training loop first-party — *new in v3.1* |
| D13 | Project navigation | **Adopt zed's `Workspace`** + `project_panel` (file tree) / `file_finder` (⌘P) / `search` (⌘⇧F), backed by `Project::local` per worktree; supersedes Spike 3's `for_buffer(None)` for IDE-affordance worktrees and absorbs LSP — *new in v3.1* |
| D14 | Keybindings | Load zed's default keymap (or curated subset): **⌘P** = quick-open, **⌘⇧F** = project search, ⌘⇧E = reveal in tree, plus tab/pane nav — *new in v3.1* |

Architecture-level questions from v1–v3 are closed. v3.1's two new directions (§3.6 navigation, §4.5–4.6 multi-agent) are **designed but unproven**; their remaining risk (R10–R12) is retired by the roadmap below (esp. Spike 5), not by more up-front design.

## 11. Current built state (what runs today)

openOrchestra today is a running macOS/Linux GPUI app that, against a real repo:

- draws the three-column chrome from generated design tokens (dark theme; light theme design exists, not yet toggled), with a native folder picker and an empty state before any repo is added;
- shows a **real, per-repo worktree sidebar** with live avatars, ahead/behind, `~dirty`, PR/CI badges (from `gh`), and a **live agent-status pill** (from the status extension → watcher);
- runs **pi's TUI in a real `terminal_view` pty** per worktree (selections/scrollback/IME), plus a plain shell on the read-only `main` entry;
- opens **real, syntax-highlighted Rust editor tabs** (no LSP yet) with a working keymap, per-worktree tab sets;
- renders the **real merge-base diff** (file list + M/A/D badges + unified preview) for the selected worktree;
- creates / archives / deletes worktrees through the real `Manager` (name input; two-click delete confirm);
- ships with `make doctor`, a macOS `.app` bundler + icon, and a cargo-audit CI workflow.

The backend crates (`worktrees`, `forge`, `status-watch`) are UI-free and fully tested (41 passing); the app links them live.

## 12. MVP → 1.0 roadmap

The spikes proved feasibility; this is the sequenced path from the current MVP to a 1.0 we'd use daily and harvest for training. Ordered by dependency and value; the two "closes the training loop" items (M6/M7) are what make the project pay for itself. Two **v3.1 tracks** — **navigation** (N-track, §3.6) and **multi-agent** (A-track, §4.5–4.6) — are interleaved below; the N-track opens with a proving **Spike 5** because it is the largest structural change since the original spikes.

**M1 — Confirm the `gh` path against a real binary (R7). ✅ DONE (July 21, 2026).** Verified against gh 2.95.0 across `cli/cli`, `microsoft/vscode`, and two CI-less repos: `--json` field shapes, all four PR states, the `pass`/`fail`/`skipping`/`pending` buckets, and both `no pull requests found …` / `no checks reported …` stderr strings all confirmed; verbatim captures committed as `crates/forge/fixtures/live/` with tests over them. **Correction:** the exit-8 convention applies to gh's *human* output only — with `--json` gh exits 0 whatever the checks did; the parser never depended on it, so this was a comment fix. **New finding:** `gh pr view <branch>` misses fork-head PRs. Full record: `docs/m1-gh-verification.md`.

**M2 — Reconcile the pi version pin (R3). ✅ DONE (July 21, 2026).** Pin is **0.73.1** everywhere (setup, doctor, `pi-ext/package.json`, extension header). The 0.70.6→0.73.1 API diff is purely additive for every surface the extension uses; R9's shared-`uiContext` mechanism re-verified intact. Re-verification found and closed a real gap: `ctx.ui.custom` blocks the agent but was not wrapped, so `custom` dialogs showed **Running** instead of **Waiting**. Extension loading also moved to upstream `jiti` 2.7, so the test harness follows it. New: `pi-ext/testing/capture-lifecycle.sh` drives a **real** pi through a full turn against a local stub provider and asserts the status transitions — run it on every bump. Mock suite 16 assertions, live capture 7. Also incidentally verified §5's `models.json` block against real pi. Full record: `docs/m2-pi-pin.md`.

**M3 — LSP / rust-analyzer in the editor (Phase D).** Construction, not a build risk (Spike 3 de-risked it). Needs a `Project::local(...)` with `LocalProjectFlags { init_worktree_trust: false, .. }` + the heavier `languages` crate + `languages::init`, then editor tabs bound to that project, and the status-bar LSP indicator lit. Resolve rust-analyzer from `$PATH`/rustup before any download. **Note (v3.1):** `Project::local` is the *same* prerequisite the navigation Workspace pivot (D13/N-track) needs, so **M3 now folds into N2** rather than shipping via a standalone `for_buffer(Some(project))` — do them as one project-model step. *Deps: N1/Spike 5; watch the disk/build-time budget (editor cone `target/` ≈ 6–7 GB).*

**M4 — Async diff/status + live refresh. ✅ DONE (July 21, 2026).** `recompute_diff` now runs on a `background_spawn` task. Requests carry a generation counter so a slow diff for a worktree the user has already navigated away from is **discarded rather than painted over the current one** — the failure mode a naive move-to-background introduces. The panel shows `Changed files …` while one is in flight instead of stale numbers.

**M5 — Worktree-lifecycle UX polish. ✅ DONE (July 21, 2026).** (a) The sidebar input now accepts optional `#branch` and `@base-ref` suffixes in any order (`fix login bug #hotfix/login @v2.1`), with a hint line so the syntax is discoverable. Chosen over the sketched `Editor::single_line` modal deliberately: same capability, far less unstable GPUI surface, and the input already exists and is already focused when you press `+`. A bare `#`/`@` stays part of the name rather than being silently swallowed. (b) Pane sets are LRU-capped at 6; dropping a set drops its pty and terminates the agent process with it, so a long day no longer leaves a dozen resident agents burning tokens on nothing.

**M6 — `agent-schema/` — the tool-schema source of truth (§4.4). ✅ DONE (July 21, 2026).** `agent-schema/v1/tools.json` is the contract, v1.0 pinned to pi's seven built-ins and dumped verbatim from pi 0.73.1's own `createAllToolDefinitions` rather than transcribed. Three consumers: `pi-ext/harness-tools.ts` (`setActiveTools` + two-way drift reporting), `crates/agent-schema` (embeds it; `MAJOR.MINOR` checkpoint compatibility — minor is additive, major refused, undeclared refused), and `training/` (stamps records). `PiRuntime` materializes it next to the extensions and points pi at it via `HARNESS_AGENT_SCHEMA` so a packaged `.app` carries the contract. See `agent-schema/README.md`.

**M7 — `training/` — session-exhaust harvesting (§6.4). ✅ DONE (July 21, 2026).** `crates/training` + the `harvest` binary. Three things it gets deliberately right: (1) the session file is a **tree**, so it resolves the *active branch* by walking `parentId` from the last record rather than reading the file in order — otherwise forked-away branches get spliced into the transcript and the model trains on paths the user rejected; (2) outcome labels are never guessed — a live worktree with no PR is `unknown`, and a `gh` failure is treated as *no information* rather than "no PR", since a transient error must not relabel merged work as abandoned; (3) records carry both the `agent-schema` version and the pi session-format version, and an unrecognized session format is **refused** rather than parsed on a guess. Parser is tested against a real captured session, plus fork / truncation / cycle cases. See `training/README.md`.

**M8 — Phase 2: RPC chat panel + `pi-client` crate (§4.2–4.3). 🟡 CRATE DONE (July 21, 2026); PANEL PENDING.** `crates/pi-client` is built and verified: protocol types checked against `rpc-types.d.ts`, strict LF-only framing, per-process reader thread with fan-out subscriptions, `SettleTracker` deriving Running/Waiting/Idle, conformance tests over verbatim recorded pi 0.73.1 JSONL, and an `--ignored` live test that drives a real `pi --mode rpc`. Building it corrected two §4.2 claims (no `agent_settled`, no `get_entries`) and showed R8's retry gap is actually closed by `auto_retry_start/end`. **Remaining:** spawn the sidecars from the agent supervisor and render the native chat tab (streaming deltas, tool cards, session tree, approval modals from `extension_ui_request`) — needs the app. Keep the Phase 1 status-file path as the fallback surface. *Deps: app build.*

### Navigation track (N) — §3.6

**Spike 5 — Zed `Workspace` embedded in custom chrome (the R10 gate). ✅ GO (July 21, 2026).** A real `Workspace`, backed by `Project::local`, is constructed **inside** openOrchestra's own chrome and verified at runtime — not just compiled. `HARNESS_SPIKE5=1 harness-app <repo>` reports `SPIKE5 OK … (1 visible worktree(s) scanned)`, so the project genuinely scans the directory rather than merely existing. **D13 is confirmed; the R10 custom-nav fallback is not needed.** Findings in §3.6.

**N1 — File explorer + ⌘P + ⌘⇧F.** On Spike 5's Workspace: dock `project_panel` (opens real worktree files), wire `file_finder::Toggle`→`cmd-p` and project search→`cmd-shift-f` via zed's default keymap (or a curated subset), scoped to the worktree's `Project`. Delivers Omar's exact ask. *Deps: Spike 5.*

**N2 — LSP on the same Project (absorbs M3).** With `Project::local` now in place from N1, bind editor items to it and add `languages`/`languages::init` for rust-analyzer + the LSP status indicator. One project-model step, not two. *Deps: N1.*

### Agent track (A) — §4.5–4.6

**A1 — `AgentRuntime` trait + registry `agent_kind`. ✅ DONE (July 21, 2026).** `crates/agent-runtime` holds the trait, the normalized `AgentSignal`/`AgentState`/`WaitReason`/`WaitFidelity`, the fan-out `Bus`, and `StatusFileSource` (one watcher for the whole app, not one per worktree). `agent_kind` is an additive `WorktreeEntry` field defaulting to `pi`, and unknown values round-trip verbatim so an older build cannot repoint a newer build's worktree at the wrong CLI. `harness-app`'s terminal pane now dispatches on it.

**This was not the pure abstraction it was planned to be — it fixed a live bug.** §4.1 states the harness copies `harness-status.ts` into each worktree so the status pill works. **Nothing in `harness-app` ever did**; `grep -r harness-status crates/harness-app/src` returned nothing. So `.harness/status.json` was never written in a real run, and the sidebar pill could only ever read `Unknown`. The entire Spike 2 pipeline was live-tested in isolation but never connected to a spawn. `PiRuntime::on_spawn` closes it, with two deliberate departures from the sketch: the extension is **embedded** (`include_str!`) so a packaged `.app` behaves like a dev build, and it is passed via **`pi -e`** from the state dir rather than copied into each worktree — both are supported pi mechanisms, and `-e` keeps a `.pi/` directory out of the user's repo and makes an extension upgrade take effect everywhere at once. Proven end to end by `tests/live_pi_pipeline.rs`, which drives a real pi through adapter → status file → bus and asserts `Running`→`Idle` plus a harvestable session path.

**A2 — OpenCode adapter (the reference peer).** Cleanest to build: start/attach `opencode serve`, subscribe `GET /event`, map `permission.updated`→`Waiting` / `session.idle`→`Idle`, reply via `POST /session/:id/permissions/:permissionID`. Proves the non-pi, non-file (SSE) path end to end. *Deps: A1.*

**A3 — Claude Code adapter.** Spawn its TUI in a pty; write a project-local `.claude/settings.json` with a `Notification` hook (matchers `permission_prompt`/`idle_prompt`, plus `Stop`) whose command shim writes the shared `.harness/status.json`. Capture `transcript_path` from the hook payload for harvesting. Pin the tested Claude Code version and feature-detect the hook schema (drifts across 2.1.x, R12). *Deps: A1.*

**A4 — Codex + Kimi adapters (terminal-first, R11).** Codex: run its TUI, set `notify = ["<shim>"]` for `agent-turn-complete`→`Idle`, mark `fidelity() = BestEffort` (optional `codex exec --json` / `codex mcp` for exact events). Kimi: run `kimi` in the terminal as the baseline (`fidelity() = TerminalOnly`); optional upgrade path is `kimi acp` (Agent Client Protocol) for structured permission/idle events. Both prove the "terminal-only / best-effort agent is a first-class case" path. *Deps: A1.*

**A5 — Notification bus + native OS notifications. ✅ DONE (July 21, 2026).** The bus landed with A1; `harness-app/src/notifier.rs` posts a native notification on any *transition* into `Waiting`, titled `<worktree> — <repo>` with the `WaitReason` as the body. Suppressed when the user is already viewing that worktree, and gated on `should_notify(fidelity)` so a best-effort adapter's turn-complete can never be dressed up as "input needed" (R11). **Delivery uses `osascript`, not `UNUserNotificationCenter`.** The native API requires a registered bundle — true for `openOrchestra.app` but *not* for `cargo run`, so the documented route would silently do nothing exactly when a developer tries to test it. `osascript` works in both with no new dependency, and AppleScript literals are escaped (a branch named `feat/"x"` would otherwise change what gets executed). Linux uses `notify-send` with `--` arg-injection hardening. *Remaining:* click-to-focus, and the D9 per-repo approval scope.

**Cross-cutting, any time:** light-theme toggle (design exists), IBM Plex bundling (Menlo is interim), resizable/collapsible panel persistence, `gh pr create` from the harness (v1.x candidate, D3-adjacent), and fork-head PR badges (`gh pr list --head <branch>` instead of `gh pr view <branch>` — M1 §5).

---

*Sources: `docs/spike2-notes.md`; `crates/harness-app/SPIKE_NOTES.md` (Spikes 1/3/4) and `README.md`; git history (`b299d6f` Spikes 1+2, `f05009e` Spikes 3+4, `baf3433` openOrchestra MVP, `aab4d3c` security pass); pi-coding-agent v0.70.6 extension API (verified) and RPC docs; zed-industries/zed crate manifests at rev `952d712d…` (v1.11.3) — navigation crates `project_panel`/`file_finder`/`search`/`workspace`; LM Studio developer docs; Claude Design project "Coding Agent Harness UI" — `docs/harness-ui/Harness.dc.html`.*

*v3.1 multi-agent research (July 21, 2026): Claude Code hooks — [code.claude.com/docs/en/hooks](https://code.claude.com/docs/en/hooks), [headless](https://code.claude.com/docs/en/headless), [Agent SDK](https://code.claude.com/docs/en/agent-sdk), [LICENSE](https://github.com/anthropics/claude-code/blob/main/LICENSE.md) (proprietary). Codex — [config-advanced](https://developers.openai.com/codex/config-advanced) (`notify` / `agent-turn-complete`), [noninteractive](https://developers.openai.com/codex/noninteractive) (`codex exec --json`), [approvals](https://developers.openai.com/codex/agent-approvals-security), [mcp](https://developers.openai.com/codex/mcp), [LICENSE](https://github.com/openai/codex/blob/main/LICENSE) (Apache-2.0). OpenCode — [server](https://opencode.ai/docs/server/) (`GET /event` SSE), [cli](https://opencode.ai/docs/cli/), [sdk](https://opencode.ai/docs/sdk/), [LICENSE](https://github.com/sst/opencode/blob/dev/LICENSE) (MIT). Kimi Code — [MoonshotAI/kimi-cli](https://github.com/MoonshotAI/kimi-cli) (Apache-2.0; command `kimi`; supports MCP + Agent Client Protocol via `kimi acp`). Caveats: Codex has no verified out-of-band approval-blocked event (R11); Claude Code hook schema drifts across 2.1.x — feature-detect and pin (R12); OpenCode event-string names confirmed via SDK `event.subscribe` shape, not enumerated inline in docs.*
