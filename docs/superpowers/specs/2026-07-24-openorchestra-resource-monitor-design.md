# openOrchestra resource monitor — design

**Date:** 2026-07-24
**Component:** `crates/harness-app` (sidebar)
**Status:** approved for planning

## Goal

Show, at the top of the left sidebar (above the WORKTREES header), how much
CPU and memory openOrchestra is using on the machine: a total with a compact
mini-graph, and — on hover — a per-worktree breakdown.

## What "openOrchestra's usage" means

The footprint is the whole set of processes the app is responsible for:

- **Base app** — the GUI process and its direct PTY children (non-tmux panes).
  Shared; not attributable to any single worktree.
- **Per-worktree** — the work each worktree's tmux sessions are doing. tmux
  sessions are named `harness_<worktree-slug>_<tab_id>` (see
  `tmux::session_name`). The real agent/shell processes run under the tmux
  **server**, a separate process tree, enumerable per session via
  `tmux list-panes -a -F '#{session_name} #{pane_pid}'`.

**Total = base app + Σ per-worktree.**

Rationale: the recommended path runs agents inside tmux, so per-worktree
attribution keys off session names. Non-tmux direct-spawn panes are children of
the GUI process and fall into the base-app bucket (acceptable: they only exist
when tmux is absent).

## Architecture

Three units, each independently testable.

### 1. Sampler (pure aggregation) — `resource_monitor::sample`

Pure function(s) over already-collected inputs so the accounting is unit-testable
without a live tmux/GUI:

```
fn aggregate(
    processes: &ProcessTable,          // pid -> { parent_pid, cpu, rss }
    app_pid: Pid,
    session_pane_pids: &[(String, Pid)], // (session_name, pane_pid) from tmux
    worktrees: &[WorktreeSlug],          // slug per worktree, from tmux::session_name inputs
) -> Snapshot
```

- Build a `pid -> children` index from `parent_pid`.
- `subtree_sum(root)` = sum of cpu/rss over `root` and all descendants (DFS,
  cycle-guarded via a visited set).
- Base app = `subtree_sum(app_pid)` **minus** any pane subtrees that happen to
  live under it (none for tmux; guards double counting).
- Per worktree `w`: for each session whose name starts with
  `harness_<w.slug>_`, sum `subtree_sum(pane_pid)`; accumulate into `w`'s bucket.
  A pane pid is counted once (visited set shared across the tick).
- `Snapshot { total_cpu, total_rss, base: Bucket, per_worktree: Vec<(WorktreeId, Bucket)> }`
  where `Bucket { cpu_pct: f32, rss_bytes: u64 }`.

Matching is by slug **prefix**, since `session_name` is lossy
(non-alphanumerics → `-`). Rare slug collisions merge into one bucket; acceptable.

### 2. Collector (background task) — `resource_monitor::spawn_sampler`

Mirrors the existing `background_executor().timer(...)` poll loop in
`workspace_host.rs`. Every **2s**:

1. `sysinfo::System::refresh_*` for processes (CPU needs two samples spaced by
   the refresh interval; sysinfo handles the delta across ticks).
2. Shell out once: `tmux list-panes -a -F '#{session_name} #{pane_pid}'`
   (skip if `tmux::has_tmux()` is false → per-worktree buckets just stay empty).
3. Call `aggregate(...)`.
4. Push `Snapshot` into shared UI state and append to a **history ring buffer**
   (60 samples ≈ 2 min) holding `(total_cpu, total_rss)` for the sparkline.
5. `cx.notify()` so the sidebar re-renders.

`sysinfo` is added to `crates/harness-app/Cargo.toml` as `sysinfo = "0.37"`
(0.37.2 is already resolved in harness-app's lockfile via the Zed cone — no new
copy). Cross-platform (macOS + Linux); no Windows.

**Build note:** always operate from `crates/harness-app` (its own workspace).

### 3. UI — top band + hover popover (`workspace/ui/sidebar.rs`)

New `render_resource_band(hx, cx)` inserted as the **first child** of the sidebar
column in `render_sidebar`, above the WORKTREES header strip. Rendered in both
the empty-repos and normal paths.

Band (~40px, `bg1`, bottom border to match the header strip):

```
 ┌───────────────────────────────────────────┐
 │ CPU  ▁▂▅▇▅▃▂  34%    MEM  ▃▃▄▄▅▅▆  2.1 GB │
 └───────────────────────────────────────────┘
```

- **Two stacked sparklines** (CPU line, MEM line), each with its own current
  label. CPU label = `%` (0–100 of one core-equivalent as sysinfo reports;
  may exceed 100 across cores — clamp display width, not value). MEM label
  auto-scales MB/GB.
- Sparkline is a small inline drawing over the history ring buffer (canvas/quad
  or a lightweight bar/line built from `div`s sized to the band). No external
  charting dep.
- Empty history / no data → flat baseline + `—` labels (never blank/flicker).

**Hover** opens a popover (same mechanism as `render_avatar_popover` /
`render_context_menu`) listing, sorted by CPU desc:

```
fix-login    18%  1.2 GB
docs-pass     9%  540 MB
App (base)    5%  380 MB
```

- One row per worktree **with non-zero usage**; zero-usage worktrees omitted.
- Final `App (base)` row for the shared GUI overhead, so rows sum to the top
  total.
- Worktree display name comes from the existing worktree/repo view data, keyed
  by `WorktreeId`.

## Data flow

```
timer(2s) ─▶ sysinfo refresh ─▶ tmux list-panes ─▶ aggregate()
   │                                                    │
   └──────────────── cx.notify() ◀── push Snapshot + history ring
                                          │
                          render_resource_band ─▶ sparklines + total
                                          └─(hover)─▶ per-worktree popover
```

## Error handling

- No tmux (`has_tmux()` false) or `tmux list-panes` errors → empty
  `session_pane_pids`; band shows base-app total only; hover shows just
  `App (base)`. No error surfaced to the user.
- A pane pid that has already exited by aggregation time → absent from the
  process table → contributes 0. No panic.
- First tick after launch may lack a CPU delta → show `—` until the second
  sample.
- Sampler failure never blocks or crashes the UI thread (runs on background
  executor; a failed tick is skipped, last snapshot retained).

## Testing

- **`aggregate` unit tests** (pure, no tmux/GUI):
  - subtree summation across a multi-level parent map.
  - a pane pid counted once even if reachable two ways; cycle guard terminates.
  - session→worktree bucketing by slug prefix; multiple sessions per worktree
    merge; unrelated (`harness_other_*`, user sessions) excluded.
  - base = app subtree minus pane subtrees (no double count).
  - total == base + Σ per-worktree (accounting invariant).
- **History ring buffer**: caps at 60, drops oldest, order preserved.
- **Memory formatting**: MB/GB scaling and rounding boundaries.
- UI render is exercised via a `Snapshot` fixture (no live sampler) to confirm
  band + popover build in both empty and populated states.

## Out of scope (YAGNI)

- Per-process drill-down inside a worktree.
- GPU/disk/network metrics.
- Historical persistence across restarts (ring buffer is in-memory only).
- Windows support.
- Configurable sample interval / graph window.
