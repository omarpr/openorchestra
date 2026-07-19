# harness-app (Spike 1)

GPUI desktop shell for omar-harness: single window, three-column skeleton
(worktree sidebar · tab strip + terminal + status bar · diff panel), with the
terminal pane running **pi's TUI** in an embedded pty. See `SPIKE_NOTES.md` for
what is real vs. mocked, and `docs/architecture.md` §3.4/§4.1/§9.

## Pinned zed revision

| | |
|---|---|
| Tag | `v1.11.3` (latest stable release tag as of 2026-07-19) |
| Commit | `952d712dac48a4af2c54fb22c82d82a9d69b72d4` |

All zed git dependencies in `Cargo.toml` use `rev = "952d712d…"`. Upgrade
deliberately and keep every zed crate on the same rev (§3.1). Note that zed's
workspace `[patch.crates-io]` entries had to be replicated in this package's
`Cargo.toml` — patches do not propagate out of a git dependency's workspace.

This package is intentionally **excluded from the repo root workspace** and has
its own empty `[workspace]` table + `Cargo.lock`, so the huge zed dependency
cone stays out of the headless crates' build graph.

## Running on macOS

Prereqs: Rust (rustup stable, ≥ 1.88), Node.js ≥ 20, git.

```sh
# 1. pi — the agent CLI the terminal pane spawns.
#    NOTE: the npm package is @mariozechner/pi-coding-agent (binary name: `pi`).
#    (@mariozechner/pi is a different tool — a vLLM pod manager, binary `pi-pods`.)
npm install -g @mariozechner/pi-coding-agent
pi --version   # verified against 0.73.1

# 2. Build and run (first build compiles ~880 crates; expect 5–15 min).
git clone <this-repo> && cd omar-harness/crates/harness-app
cargo run                          # cwd for pi = current directory
# or point the terminal at a real worktree:
cargo run -- /path/to/some/worktree
# or: HARNESS_WORKTREE_ROOT=/path/to/worktree cargo run
```

Expected window: dark three-column layout titled "omar-harness" —

- **Left**: "WORKTREES" sidebar, two repo groups (`omar-harness`, `pi`) with
  three hardcoded worktree cards (initials avatar, name, mono branch label,
  ahead/behind + dirty counts, and a colored agent-status pill:
  green **Running** / amber **Waiting** / gray **Idle**). Clicking a card
  selects it and updates the breadcrumb + status bar.
- **Center**: a tab strip with one `❯_ terminal` tab; below it the terminal
  running pi's TUI (or your `$SHELL` if `pi` isn't on PATH — the status bar's
  right side shows which was spawned); bottom status bar shows
  `<agent state> · <branch>`.
- **Right**: diff panel placeholder with a hardcoded changed-files list
  (M/A/D badges, +adds/−dels, totals) and a "unified diff view — Spike 4" slot.

Type in the terminal to drive pi (model setup per §5: point pi at LM Studio via
`~/.pi/agent/models.json`, then `/model` inside pi). `cmd-q` quits.

Known spike limitations: no scrollback UI (pi's TUI is alt-screen, so this
rarely matters), no per-cell background colors / underline styles, no IME, no
mouse reporting to the pty, single terminal tab, no light theme.

## Linux

Builds and checks on Linux (`cargo check` + `cargo build` clean). In a
display-less container the app still runs (gpui falls back to a headless
platform) and demonstrably spawns `pi` in its pty.
Needs the usual zed build deps (`libxkbcommon-dev libxkbcommon-x11-dev
libwayland-dev libx11-xcb-dev libvulkan1 mesa-vulkan-drivers pkg-config
libssl-dev libzstd-dev cmake clang` — see zed's `docs/src/development/linux.md`).
Not yet run against a live display server.

## pi binary notes

- npm package: `@mariozechner/pi-coding-agent` (bin name `pi`); installs to
  `$(npm prefix -g)/bin/pi` (verified in the build container at
  `/home/claude/.npm-global/bin/pi`, v0.73.1).
- The pane resolves `pi` by scanning `$PATH` at launch; if missing it falls
  back to `Shell::System` (your login shell) so the terminal still works.
