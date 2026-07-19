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
