# Spike 1 notes — GPUI shell + terminal + pi

Date: 2026-07-19 · Owner: spike agent · Status: **landed** (`cargo check` and
`cargo build` clean on Linux; app also *executed* in the headless container —
gpui_linux falls back to a headless platform — and `pstree` confirmed
`harness-app → pi` running as the pty child. Not yet run against a real
display; see README.md for the macOS instructions).

## What was proven

1. **gpui is consumable out-of-tree at a pinned rev.** Pinned `v1.11.3`
   (`952d712dac48a4af2c54fb22c82d82a9d69b72d4`). Note: as of ~v1.11 gpui split
   into `gpui` + `gpui_platform` (+ per-OS `gpui_macos`/`gpui_linux`/…); the
   entry point is now `gpui_platform::application().run(...)`, not
   `gpui::Application::new()`. Older blog/example snippets are stale.
2. **Zed's `terminal` crate works out-of-tree** (§3.3 primary path — no
   alacritty fallback needed). Its dependency cone (`settings`, `theme`,
   `theme_settings`, `task`, `util`, `release_channel`, zed's `alacritty_terminal`
   fork) compiles cleanly from the git rev. Required app-side init, in order:
   `release_channel::init(version, cx)` → `settings::init(cx)` →
   `theme_settings::init(theme::LoadThemes::JustBase, cx)`. Settings types
   self-register via `inventory`, so no per-crate registration calls.
3. **Zed's workspace `[patch.crates-io]` does not propagate** to out-of-tree
   consumers. Replicated the entries in our cone (`async-process`, `async-task`,
   `calloop`, `notify`, `notify-types`) in this package's `Cargo.toml`. This is
   a standing upgrade-time chore: re-diff zed's root `Cargo.toml` on every rev
   bump.
4. **pi spawn wiring.** `TerminalBuilder::new(Some(worktree_root), …,
   Shell::WithArguments { program: <pi from $PATH>, .. }, …)` with fallback to
   `Shell::System`. The worktree root comes from argv[1] /
   `$HARNESS_WORKTREE_ROOT` / cwd. Verified end-to-end in the container: with
   the app running headless, `pi` shows up as a live child process of
   `harness-app` (npm package **`@mariozechner/pi-coding-agent`**, v0.73.1,
   bin `pi`). ⚠️ Deviation from the plan: `@mariozechner/pi`@0.70.6
   on npm is a *different tool* (vLLM pods CLI, bin `pi-pods`) — the
   architecture doc's references to "pi" map to `pi-coding-agent`.
5. **Three-column skeleton** per §3.4 with the design tokens as a
   `HarnessTheme` struct (`bg0..2`, `surface*`, `tx0..2`, `accent`,
   green/red/amber, border, IBM-Plex-ish font slots). Sidebar cards carry the
   full anatomy (initials avatar, name, mono branch, ahead/behind, `~N` dirty,
   status pill Running/Waiting/Idle) and are click-selectable; status bar
   mirrors the pill + branch.

## Terminal approach chosen: zed `terminal` crate + custom minimal view

- **Model layer = zed `terminal` crate** (pty, alacritty state machine, gpui
  event integration). This is what §3.3 wanted, and it survived out-of-tree.
- **View layer = ~150-line custom renderer** (`src/terminal_pane.rs`): rows of
  `StyledText` runs built from `Terminal::last_content().cells`, fg colors
  mapped through the zed base theme + harness tokens, block-cursor overlay,
  canvas-based size probe driving `Terminal::set_size`, key input via
  `try_keystroke` + raw `input()` for printable chars.
- Why not zed's `terminal_view` (the full renderer): it depends on `workspace`,
  which drags roughly half of zed (project, client, fs watchers, …). That cone
  is Spike 3's problem (it comes with `editor` anyway). If Spike 3's editor
  lands, we should revisit swapping this custom view for `terminal_view` to get
  selections, hyperlink hover, scrollback UI, bg colors, IME "for free".
- Consequence of the custom view (spike-acceptable, listed in README): no
  per-cell backgrounds/underline, no mouse reporting, no scrollback scrolling,
  no IME. pi's TUI is alt-screen and keyboard-driven, so it remains usable.

## What's mocked

- Sidebar repos/worktrees/cards, PR/CI-ish counters, diff-panel file list and
  totals: hardcoded in `src/model.rs` (Spikes 2/4 wire status watcher, `gh`
  poller, registry, git).
- One terminal tab only; tab strip and "+ new" are static chrome.
- Dark theme only; no resizable/collapsible panels yet.
- Agent status pill is static (Spike 2's `.harness/status.json` watcher drives
  it for real).

## Risks confirmed / updated (vs §8)

- **R1 (zed dep cone, unstable APIs) — confirmed but tractable.** The
  terminal-level cone is ~880 crates with this package's lockfile; full
  `cargo check` from cold ≈ minutes, incremental is fine. API churn is real
  (gpui_platform split happened *between* v1.10 and v1.11) — pinning + reading
  `crates/gpui/examples` at the pinned rev is the working method. Spike 3
  (editor) remains the real test; nothing seen here contradicts its plan.
  - New sub-risk: the `[patch.crates-io]` replication chore (above). Low
    effort, but silent breakage if forgotten on upgrade.
- **R6 (thin docs / theme mapping) — confirmed, mitigated.** Zed source *is*
  the documentation; the pinned shallow clone was consulted constantly.
  Design-token → zed-theme generation not yet attempted (Spike 3); for now the
  terminal maps ANSI colors through zed's base dark theme
  (`LoadThemes::JustBase`) with harness fg/bg token overrides, which looks
  coherent.
- **R2 (GPL) — now actual**: linking `terminal` makes this binary
  GPL-3.0-or-later (recorded in Cargo.toml `license`). Already accepted in §3.2.
- **R4 (Node per worktree) — unchanged**; pi resolved from PATH at launch,
  graceful `$SHELL` fallback verified in code (container had pi installed).
- New: **v1.11.x is one week old.** If macOS-specific breakage appears at this
  rev, drop back to `v1.10.3` (`0c54c414d522234de7298039708ffe85a116892a`) —
  but that predates the gpui_platform split, so `main.rs`'s entry point would
  need the old `Application::new()` form.

## Open items for Spike 3 (editor pane)

- `editor` drags `project`/`language`/`lsp` — expect the workspace-sized cone
  the terminal avoided; budget disk (this package's `target/` is already
  ~5 GB with `debug = 0`) and compile time accordingly.
- Check whether `editor` requires more globals than
  settings/theme/release_channel (likely: `language::init`, fs, node runtime
  for LSP servers) and whether any of them fight our standalone-package setup.
- Theme generation from `HarnessTheme` tokens → zed `ThemeSettings` override
  (R6) so editor + terminal + chrome share one palette.
- Revisit `terminal_view` adoption once `workspace` is in the graph anyway.

## Files

- `Cargo.toml` — standalone package (own `[workspace]`), zed rev pin, patch
  replication, `debug = 0`.
- `src/main.rs` — entry, zed-crate global init order, window setup, quit keys.
- `src/theme.rs` — `HarnessTheme` design tokens (§3.4).
- `src/model.rs` — mocked repos/worktrees/changed files.
- `src/workspace.rs` — three-column layout, sidebar cards, tab strip, status
  bar, diff placeholder.
- `src/terminal_pane.rs` — pi spawn + custom terminal renderer (see above).

---

# Spike 3 notes — Zed editor pane + theme mapping

Date: 2026-07-19 · Status: **GO** (`cargo check` + `cargo build` + link clean;
app executed headless with no panic; theme-generation unit test passes). This is
the §9 go/no-go gate for §3.2 (embedding zed's `editor`). **Verdict: proceed —
R1 retired for the editor path.**

## What was proven

1. **Zed's `editor` crate is consumable out-of-tree at the pinned rev
   (`v1.11.3`), with ZERO patch-list changes.** Adding `editor` (+ `language`,
   `grammars`, `tree-sitter-rust`) to the standalone package compiled the entire
   cone — `workspace`, `project`, `lsp`, `dap`, `db`, `multi_buffer`, `client`,
   `node_runtime` all build transitively — in ~4 min cold. The `[patch.crates-io]`
   set from Spike 1 was sufficient unchanged. **This was the whole risk (R1); it
   passed.**
2. **A real `Editor` renders as an embedded pane WITHOUT the `workspace` crate
   at runtime.** `Editor::for_buffer(buffer, None /* no Project */, window, cx)`
   (`editor.rs:1781`). `editor::init(cx)` is called (safe — sets defensive
   globals + workspace item hooks) but is not required for `for_buffer`.
3. **Out-of-tree Rust syntax highlighting with minimal deps.** We do NOT pull the
   heavy `languages` crate (all ~18 grammars + LSP adapters + `pet*`). Instead:
   the light `grammars` crate (rust-embed of zed's `config.toml` + `.scm` query
   files; depended on WITHOUT its `load-grammars` feature) + `tree-sitter-rust`
   directly, so we compile **one** grammar. Build path
   (`src/editor_pane.rs::build_rust_language`):
   `Language::new(grammars::load_config("rust"), Some(tree_sitter_rust::LANGUAGE.into()))
   .with_queries(grammars::load_queries("rust"))` then
   `language.set_theme(active.syntax())` to bind capture IDs → colors, then
   `Buffer::set_language(...)`. Parsing runs on the gpui `BackgroundExecutor`.
4. **Design-token → zed-theme generation (R6).** `src/harness_zed_theme.rs`
   builds a `ThemeFamilyContent` from the `HarnessTheme` tokens via a `json!`
   value (every content field is `Option`/`#[serde(default)]`, so a partial
   object deserializes and `theme_settings::refine_theme_family` fills the ~100
   other color fields with coherent dark defaults), registers it in the
   `ThemeRegistry`, and activates it with `GlobalTheme::update_theme`. **Because
   both `editor` and `terminal` read `cx.theme()`, one token set now drives
   chrome + editor + terminal + syntax** — the R6 goal. Verified by a pure-fn
   unit test (`tokens_map_into_resolved_theme`): tokens survive the round-trip
   into the resolved theme's `editor_background`, `terminal_ansi_*`, and syntax
   captures (`keyword`=accent, `string`=green).
5. **UI:** the center pane now has two real, clickable tabs — `❯_ terminal`
   (Spike 1) and `{} editor` (Spike 3) — with focus handed to the active pane on
   switch. Editor opens a real worktree file (`src/main.rs` / `crates/harness-app
   /src/main.rs`), falling back to a bundled sample so it always shows highlighted
   Rust.

## Deferred (as designed — mirrors Spike 1's terminal scope)

- **rust-analyzer / LSP — Phase D, not yet wired, but de-risked.** The full
  `project`+`lsp`+`client`+`node_runtime` cone already compiles (it's under
  `editor`), so this is now *construction*, not a build risk. Minimal path
  (verified against source): `Project::local(client, node, user_store,
  languages, fs, None, flags, cx)` with `Editor::for_buffer(buf, Some(project),
  …)` — the editor auto-registers the buffer with `LspStore` and holds the LSP
  handle. rust-analyzer resolves from `$PATH`/rustup before any download.
  **Gotcha found (corrects the exploration note):** at this rev
  `LocalProjectFlags::default()` has `init_worktree_trust: **true**` — you MUST
  pass `LocalProjectFlags { init_worktree_trust: false, .. }`, otherwise LSP
  stalls on a trust prompt that has no UI. Also needs the heavier `languages`
  crate (all grammars) and `languages::init(languages, fs, node, cx)`.
- **Editing keymap.** Typing flows through the editor's IME input handler; arrow
  keys / backspace / selection need a keymap bound (or zed's default keymap
  loaded). Deferred to the MVP chrome pass. Mouse click-to-place-cursor works.
- **`terminal_view` swap.** Now that `workspace` is in the graph anyway, revisit
  replacing Spike 1's custom terminal view with zed's `terminal_view` for
  selections/scrollback/IME "for free" (Spike 1 open item).

## Risks confirmed / updated (vs §8)

- **R1 (zed `editor` cone) — RETIRED for the editor path.** Compiled out-of-tree
  at the pinned rev with no patch changes; renders headless without panic. The
  fallback custom-editor plan is no longer needed. (Ongoing: the standing
  `[patch.crates-io]` re-diff chore on every rev bump still applies — R1
  sub-risk from Spike 1.)
- **R6 (token→theme mapping) — RESOLVED.** One token set generates the zed theme
  for all panes; unit-tested. Syntax hues not in the chrome token set (purple
  functions, teal types, orange numbers) are currently literals in
  `harness_zed_theme.rs` — extending `HarnessTheme` with a first-class syntax
  ramp is the tidy follow-up.
- **R2 (GPL) — unchanged/now broader**: linking `editor` (GPL-3.0-or-later)
  reinforces the binary's GPL status (already accepted, §3.2).
- **Disk/build:** the package `target/` grows to ~6–7 GB with the editor cone
  (`debug = 0`); cold `cargo build` ≈ 10 min in-container. Incremental is fine.
- New: **`recursion_limit = "256"`** needed at the crate root — the harness-theme
  `json!` literal nests past the default 128.

## How to run / test (Spike 3)

```sh
cd crates/harness-app
cargo run                    # terminal tab (pi) + editor tab (highlighted Rust)
cargo test                   # includes tokens_map_into_resolved_theme (theme gen)
# Container-only: needs libxkbcommon-dev + libxkbcommon-x11-dev to LINK on Linux
# (gpui x11/wayland); irrelevant on macOS.
```

## Files (Spike 3)

- `src/editor_pane.rs` — `EditorPane` view: builds the Rust `Language`, opens a
  worktree file into a `Buffer`, `Editor::for_buffer(.., None, ..)`, renders.
- `src/harness_zed_theme.rs` — token → `ThemeFamilyContent` → active `Theme`
  (`install`); `theme_family_content` is the pure, unit-tested core.
- `src/main.rs` — installs the harness theme + `editor::init` before window open;
  `recursion_limit`.
- `src/workspace.rs` — two-tab center pane (terminal · editor) with focus
  routing.
- `Cargo.toml` — `editor`/`language`/`grammars`/`tree-sitter-rust`/`serde_json`.

---

# Spike 4 notes — Worktree vertical slice

Date: 2026-07-19 · Status: **landed** (headless crate fully real-git-tested;
app builds + runs headless with no panic against the real repo). Completes the
§9 spike plan.

## Headless crate: `crates/worktrees` (the tested core)

UI-free, in the root workspace (fast build), mirroring `forge`/`status-watch`.
Three layers:

- **`Registry`** (`registry.rs`) — the on-disk index (§6.1). Atomic JSON
  persistence (write-temp + rename); `WorktreeEntry` = repo, root, branch, slug,
  editable name, path, session file, avatar (initials + palette color), PR
  number, created_at, archived. Renames/avatar edits mutate the registry only,
  never git (§6.1). `grouped_by_repo` powers the D4 sidebar; created_at is
  caller-supplied so the crate has **no clock** (deterministic tests).
- **`GitWorktrees<R>`** (`git.rs`) — git ops behind the same injectable
  `CommandRunner` as forge: `worktree add/remove`, `branch -D`, `worktree list`,
  `default_branch` (origin/HEAD → local main/master probe), `status` (porcelain
  dirty count + `rev-list --left-right` ahead/behind), and `diff` against
  `merge-base(base_ref, HEAD)` → per-file numstat + name-status (M/A/D/R) +
  unified text (§6.1 D10). Diff is taken merge-base → **working tree**, so it
  shows committed *and* uncommitted task changes. Unresolvable base → empty diff,
  never an error.
- **`Manager<R>`** (`lib.rs`) — ties them together for create / archive / delete
  (§6.1 D6): git op first, then registry update + persist, so a git failure
  never leaves a phantom entry. Archive keeps branch + entry (marked archived,
  §6.4); delete removes worktree + branch + entry.

**Tests (real git, unlike Spike 2's fixture-only gh):** `tests/git_e2e.rs`
spins up temp repos and drives the full lifecycle — create two worktrees, make
committed + uncommitted changes, assert ahead/behind/dirty, assert the
merge-base diff (files, M/A/D, adds/dels, unified text), archive (dir gone,
branch kept), delete (dir + branch + entry gone), and registry persistence
across reload. `tests/registry.rs` covers add/dup/rename/avatar/archive/group/
atomic-persist. **8 tests, all pass; workspace total now 41.**

## App integration (`crates/harness-app`)

- **`data.rs`** — `WorkspaceData::build` turns the registry + live git into the
  view model (replaces Spike 1's `model.rs` mocks). Always seeds the primary
  repo (the one the app launched in, via `git rev-parse --show-toplevel`) so its
  read-only `main` entry shows even with zero worktrees. `compare_ref` =
  `origin/<default>` when the remote ref exists, else local `<default>`.
- **`workspace.rs`** (rewritten) — sidebar from `data.repos` (real avatars +
  ↑ahead ↓behind ~dirty); **per-worktree tab sets (D5)** via a
  `HashMap<selection-key, PaneSet>` cache built lazily on first selection (so we
  don't spawn N pis up front); diff panel renders the selected entry's real
  merge-base diff (file list + M/A/D badges + a capped unified preview);
  `+ new worktree`, **Archive**, and **Delete (two-click confirm)** call the real
  `Manager`. The `main` entry gets a plain shell (no pi) via a new
  `TerminalPane::new(_, spawn_agent, _)` flag (§6.1 D8).
- Harness state lives in `<repo_parent>/.omar-harness/` (`registry.json` +
  `worktrees/`) so worktrees don't nest inside the repo.

## Verified

- `cargo test` at root: **41 pass** (worktrees 8, forge 23, status-watch 9,
  +1 doc). `cargo check`/`build` on harness-app clean.
- Headless run against the real `omar-harness` repo: no panic — registry load,
  `WorkspaceData::build` (real `git rev-parse`/`default_branch`/`status`), pane
  creation, and the `main` merge-base diff all execute. (gpui headless fallback,
  same bar as Spikes 1 & 3.)

## Deferred / follow-ups

- **Agent-status pill is still a static placeholder.** Wiring Spike 2's
  `status-watch` (`.harness/status.json` → the pill) into the app is the clean
  next integration — the pipeline exists, it's just not connected to the sidebar.
- **Create UX is a generated slug** (`task-N`). A proper new-worktree dialog
  (name/branch/base input) wants a text field — the editor crate is now in-tree,
  so an `Editor::single_line` input modal is the natural approach.
- **Diff/status are computed synchronously on selection.** Fine for a spike;
  move to a background task + the forge poller / a status watcher for live
  refresh in the MVP (avoids blocking the UI on `git diff` for large repos).
- Per-worktree panes cache but never evict; archived/deleted panes are dropped,
  but long sessions with many worktrees will hold many pi processes — add an LRU
  or explicit close in the MVP.

## Files (Spike 4)

- `crates/worktrees/src/{lib,registry,git,runner}.rs` + `tests/{registry,git_e2e}.rs`
- `crates/harness-app/src/data.rs` (new), `workspace.rs` (rewritten),
  `model.rs` (trimmed to `AgentStatus`), `terminal_pane.rs` (`spawn_agent` flag),
  `main.rs` (`mod data`).
