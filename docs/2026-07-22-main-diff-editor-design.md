# Design — First-class main, live diff, native editor affordances

**Status:** Approved design · July 22, 2026
**Scope:** Three user-requested improvements to `crates/harness-app`, implemented as four
separable deliverables against the existing `workspace.rs` / `workspace_host.rs` seam.
**Relationship to the architecture doc:** this refines §3.4 (sidebar, diff panel),
§3.6 (project navigation), and §6.1/D8 (main checkout). It does not change any
architectural decision; it finishes wiring that D8-revised, N-track, and M4 left partial.

---

## Motivation

Three things the harness gets wrong today, from the user:

1. **Worktrees are privileged over the main checkout.** IDE/RUNNING affordances and a
   clear "this is selected" indicator are worktree-only. Engineers making a single change
   should not be forced into a worktree.
2. **The diff viewer lags behind the agent.** It shows stale numbers while an agent edits,
   until you re-select the worktree.
3. **The embedded Zed editor is under-wired.** ⌘P / ⌘⇧F only fire once a file is open;
   there is no native git/diff view; and Markdown has no good rendered display mode.

The exploration behind this design (three read-only passes over the code) established that
**the backend is largely ready for all three** — the work is overwhelmingly UI-layer wiring,
not new subsystems.

---

## Deliverable A — Live diff refresh

### Problem
`recompute_diff` (`workspace.rs:1302`) is event-driven only: it runs on startup, on sidebar
selection (`select()`), and after structural mutations (create/rename/archive/delete). Nothing
observes source-file writes, so during an agent turn the diff panel is frozen at whatever it
computed the last time the worktree was selected. The async plumbing (M4 generation counter)
already exists — it simply has no trigger connected to "files changed on disk."

### Approach — a per-selection filesystem watcher
Watch **only the currently-selected** worktree/main root (bounds cost to the one diff on
screen). On selection change, tear down the old watch and start a recursive `notify` watch on
the new root. On any qualifying event, debounce and recompute.

- **Debounce:** ~300 ms trailing debounce. A burst of writes (an agent editing several files
  in a turn) collapses to one recompute after the burst settles.
- **Ignore filter:** drop events whose path contains `/.git/`, `/target/`, or
  `/node_modules/`. Without this, a `cargo build` churns `target/` and would trigger a
  continuous recompute storm. (Git itself ignores gitignored files in the diff output, so a
  missed ignore is *correctness-safe* but *cost-unsafe* — hence the filter is about cost.)
- **Staleness:** the existing `diff_gen` counter (`workspace.rs:395`, checked at `:1325`)
  already discards a recompute for a worktree the user has navigated away from. The watcher
  reuses it; no new staleness logic.
- **Canonicalized paths:** the watcher must resolve its root once and key on the canonical
  form. macOS FSEvents reports `/private/var/...` while callers pass `/var/...`; this exact
  mismatch silently broke `status-watch` on the first real macOS run (architecture §9,
  July 21). We do not repeat it.

### Data flow
```
agent writes file
  → notify fs-event (raw)
  → ignore-filter + 300ms trailing debounce
  → send DiffDirty(root) on a crossbeam channel
  → drained in workspace.rs (same poll pattern as the forge/status/nav channels)
  → recompute_diff (background_spawn, diff_gen guard)
  → panel repaints
```

### Where it lives
A focused watcher — either a new small module in `harness-app` or a generic recursive
path-watch added to `crates/status-watch` (the plan chooses; `status-watch`'s `notify`
setup is the reference). It emits a "root changed" signal on a channel; the workspace drains
it on a short timer exactly like the existing forge/status/nav drains
(`workspace.rs:632/648/664`). Selection changes (in `select()`, `workspace.rs:1336`)
retarget the watch.

### Out of scope (A)
Live sidebar `~dirty` counts for *non-selected* worktrees. The user asked for the diff
*viewer*; per-card dirty counts stay on their current refresh (build/refresh). Noted as a
possible follow-up, deliberately not built here (YAGNI).

---

## Deliverable B — Native editor affordances

All three sub-parts converge on one seam: the hosted Zed `Workspace` wrapped in
`workspace.actions(div(), …).key_context("Workspace")` (`workspace.rs:3092`), together with
the App-level `cx.observe_new::<Workspace>` hooks each Zed `init()` installs. **B1 lands
first** because B2 and B3 inherit its focus gate.

### B1 — ⌘P / ⌘⇧F without a file open (focus fix)
**Root cause (not lazy construction — everything is built eagerly):** the ⌘P / ⌘⇧F bindings
are `Scope::Workspace` (`shortcuts.rs:167-176`), so they only match when the focused element's
context stack contains `"Workspace"`. That context is supplied by the wrapper at
`workspace.rs:3092`. With no file open, focus in practice rests on the harness root
(`key_context("Harness")`, `workspace.rs:4157`) rather than inside the hosted-workspace
subtree, so the keystroke walks a stack of only `Harness`/global and never matches. Opening a
file moves focus into the `Editor` item — inside the wrapper — and the bindings start firing.

**Fix:** when the Editor tab becomes active, place focus on the hosted Workspace's active pane
(`ws.read(cx).focus_handle(cx)`), which even when empty carries `Workspace > Pane > EmptyPane`
(zed `pane.rs:4292`). Ensure a click into an empty editor region also lands focus there.
Optionally also make the two bindings resolve when focus is on the docked `project_panel`
(also inside the Workspace subtree, so this may come for free once focus is placed correctly).

**Must not regress:** focus is only nudged when the **Editor** tab is active — never stealing
focus from the terminal or pi-chat tabs.

*Files:* `workspace.rs` — `focus_active_pane` (`:1104`) / tab-activation path, and the
`TabContent::Editor` render arm (`:3079`).

### B2 — Zed-native git/diff (full `git_ui`)
Add zed's `git_ui` crate — the complete native git surface: **project-diff multi-file view**,
**git panel** (stage/unstage), and **in-editor gutter change-bars**. This is *additive*: the
custom diff/review panel (`diff_review.rs` + `render_diff_panel`) stays, because it carries the
review-comment → hand-back-to-agent flow and per-file reviewed checkboxes that git_ui does not.

- **Cargo:** add `git_ui` (and explicit `buffer_diff`, currently only transitive) to
  `harness-app/Cargo.toml`, pinned to the same zed rev `952d712` (v1.11.3) as the rest of the
  cone.
- **Init:** call `git_ui::init(cx)` in `main.rs` beside the existing inits (`:159-231`). It
  registers via `cx.observe_new::<Workspace>` (zed `git_ui.rs:82`), so it auto-attaches to the
  hosted Workspace with **no per-instance wiring**. It also installs `editor::set_blame_renderer`
  and per-editor conflict views.
- **Keybinding:** add a `Scope::Workspace` binding for the project-diff action
  (`git::Diff` / `OpenModifiedFiles`, `git_ui.rs:296`) to `shortcuts.rs`. The view opens as a
  **pane item**, not a modal — no new modal host required (unlike FileFinder).
- **Gutter change-bars:** come from the editor reading `BufferDiff` off the hosted `Project`'s
  git store; verify the hosted `Project::local` tracks git status (it should, given the repo
  root is a worktree root).

**Gotchas:**
- **`[patch.crates-io]` re-diff (R1a).** Adding `git_ui` may pull deps that zed patches at its
  root. Re-diff zed's root `Cargo.toml` against `harness-app/Cargo.toml`'s replicated patch set
  after adding, per standing discipline (architecture §3.1).
- **Build-time / disk budget.** The editor cone `target/` is already ~6–7 GB; `git_ui` is the
  heaviest of the three adds (~88 dep lines). Acceptable, but expect a long cold build.
- License: already GPL-3.0-or-later; no change.

### B3 — Markdown display mode (native `markdown_preview`)
Add zed's `markdown_preview` crate — a **live, code-highlighted rendered preview** that opens
as an editor pane and tracks the active buffer (not disk).

- **Cargo:** add `markdown_preview` (same zed rev).
- **Init:** `markdown_preview::init(cx)` in `main.rs`. Registers via `cx.observe_new::<Workspace>`
  (zed `markdown_preview.rs:37`) — auto-attaches to the hosted Workspace.
- **Keybindings:** bind `zed_actions::preview::markdown::OpenPreview` and `OpenPreviewToTheSide`
  (`Scope::Workspace`) in `shortcuts.rs`. Opens as pane items, so they route through the
  existing `workspace.actions()` wrapper.
- **Custom preview stays:** `markdown_preview.rs`'s `MarkdownPreview` remains the diff-panel
  `◧` toggle (`workspace.rs:4082`) — that serves the review flow (disk content of a diffed
  file), which is a different job from previewing an open editor buffer.

---

## Deliverable C — First-class main/master

### Problem
Main is already ~80% first-class: `Selection::Main(repo)` (`workspace.rs:226`), a real pane set
keyed `main:<repo>` (`workspace.rs:856`), editor/terminal tabs, agent spawning (D8 revised,
architecture §10 D8 note / `workspace.rs:974`), the diff panel, and the status bar all work
generically. The gaps are all in the sidebar row and one watch registration:

- `render_main_entry` (`workspace.rs:2745`) renders a **hardcoded green `✓`** — never the live
  `agent_pill` — and the repo root is **never registered** with `StatusFileSource`
  (`workspace.rs:474-478`, `refresh` `:1368-1379`), so an agent on main can show no live state
  and fire no notification.
- Selection highlight on the main row is **background-only** (`:2763`); worktree cards get an
  accent left-border (`:2898`). This is the "make visible which is selected" gap.
- No ahead/behind/dirty/CI badges — `RepoView` (`data.rs:22`) lacks the fields.

### Approach — full parity
1. **Live status pill + notifications.** Register each `repo_root` with `StatusFileSource`
   (the watcher setup at `workspace.rs:474-478` and in `refresh`), so `.harness/status.json`
   written under the repo root routes to `agent_status[repo_root]`. Render
   `agent_pill(self.agent_status.get(repo_root))` in `render_main_entry` instead of the static
   `✓`. This automatically lights the existing notifier path for agents on main. The adapter
   `on_spawn` path already drops `.harness/` and excludes it via the repo's local git excludes
   (common dir — covers main), so no repo-dirtying.
2. **Selection highlight.** Give the main row the same accent left-border as worktree cards
   when `Selection::Main(repo)` is active.
3. **Badges.** Add `dirty`, `ahead`, `behind`, and `ci` to `RepoView` and compute them in
   `new_repo_view` (`data.rs:74`): `git.status` (porcelain dirty count),
   `rev-list --left-right origin/<default>...HEAD` (ahead/behind), forge
   `checks_for_branch(default_branch)` (CI). Render them in `render_main_entry`. **No PR badge**
   — a PR is a feature-branch concept; main is the target, not a PR head.

### Explicitly unchanged
- **pi-chat stays worktree-only.** It is pi's RPC/training surface wired to a worktree session
  (`workspace.rs:981`); running it on main is intentionally blocked and stays blocked.
- Right-click archive/delete stays absent from main (there is nothing to archive/delete).

*Files:* `data.rs` (`RepoView` fields + `new_repo_view`), `workspace.rs`
(`render_main_entry`, status-watch registration).

---

## Sequencing

`A → C → B1 → B2 → B3`. A and C are independent and self-contained; B1 is a prerequisite for
B2/B3 feeling reliable, so the editor sub-parts are ordered. Each deliverable is separable and
independently shippable.

## Testing strategy

- **A (watcher):** unit-test the ignore-filter and debounce logic headlessly (path → keep/drop,
  burst → single emit). The canonical-path keying gets a symlink regression test, mirroring the
  one status-watch already has. End-to-end diff repaint is verified by running the app against a
  repo and editing a file.
- **B1 (focus):** verified via the existing `HARNESS_PROBE_KEYS` / `probe_finder` harness in
  `workspace_host.rs`, which dispatches real actions through the real focus path — assert ⌘P
  opens the finder and ⌘⇧F lands a search item **with no file open**.
- **B2/B3 (git_ui / markdown_preview):** compile clean at the pinned rev with the `[patch]` set
  re-diffed; probe that the actions dispatch onto the hosted Workspace (open project-diff, open
  markdown preview). `cargo check`/`build` clean + clippy-clean is the gate; runtime confirmation
  in the app.
- **C (main parity):** `RepoView` field computation is unit-testable against a real repo (the
  `worktrees` crate already has real-git tests). Pill/badge rendering confirmed in the app.

The backend crates stay UI-free and fully tested (architecture §11: 130 headless tests);
anything testable without a window gets a test, and the whole tree stays clippy-clean.

## Risks

| Risk | Mitigation |
|---|---|
| Watcher event storm during builds | Ignore-filter (`.git`/`target`/`node_modules`) + 300 ms debounce + `diff_gen` guard |
| FSEvents canonical-path mismatch (repeat of the §9 bug) | Resolve the watched root once; key on the canonical form; symlink regression test |
| `git_ui` pulls new patched deps | Re-diff zed's root `Cargo.toml` `[patch.crates-io]` set after adding (R1a) |
| Build time / disk from the added cone | Accepted; expect a long cold build |
| Focus fix steals focus from terminal/chat | Nudge focus only when the Editor tab is active |
