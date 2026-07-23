# First-class main, live diff, native editor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the main checkout a first-class citizen, refresh the diff viewer live as agents edit, and finish wiring the embedded Zed editor (⌘P/⌘⇧F without a file, native git/diff, native Markdown preview).

**Architecture:** All UI-layer wiring against the existing `crates/harness-app` seam (`workspace.rs`, `workspace_host.rs`, `data.rs`, `shortcuts.rs`, `main.rs`). A new per-selection filesystem watcher drives the existing async `recompute_diff`. Two Zed crates (`git_ui`, `markdown_preview`) are added and initialized so they auto-attach to the hosted `Workspace` via `cx.observe_new`. Main parity reuses the existing `Selection::Main` model, status pipeline, and `git.status`.

**Tech Stack:** Rust, GPUI (zed rev `952d712`, tag v1.11.3), `notify` (fs events), `crossbeam` channels, zed crates `git_ui`/`markdown_preview`/`buffer_diff`.

## Global Constraints

- Zed crates pinned to git rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4` (tag `v1.11.3`) — same rev as the existing cone. Copied verbatim from architecture §3.1.
- After adding any zed crate, **re-diff zed's root `Cargo.toml` `[patch.crates-io]`** against `harness-app/Cargo.toml`'s replicated set (R1a standing chore).
- Filesystem watchers **must key on canonicalized paths** — macOS FSEvents reports `/private/var/...`; keying on the caller path drops every event (architecture §9 bug).
- The binary is already GPL-3.0-or-later; adding GPL zed crates changes nothing.
- Keep the tree **clippy-clean** and all headless tests passing (architecture §11).
- Do **not** dirty the user's repo: anything dropped under a repo root (`.harness/`) is excluded via the repo's local git excludes (already handled by the adapter `on_spawn` path).
- pi-chat stays worktree-only — do not enable it on main.

---

## File structure

| File | Responsibility | Change |
|---|---|---|
| `crates/harness-app/src/diff_watch.rs` | Pure watch-path filter + `notify` glue emitting root-changed signals | Create |
| `crates/harness-app/src/workspace.rs` | Own the watcher, retarget on select, drain + recompute; main sidebar parity render; focus fix | Modify |
| `crates/harness-app/src/data.rs` | `RepoView` git-status fields (dirty/ahead/behind/ci) | Modify |
| `crates/harness-app/src/main.rs` | `git_ui::init` + `markdown_preview::init` | Modify |
| `crates/harness-app/src/shortcuts.rs` | Keybindings for project-diff + markdown preview | Modify |
| `crates/harness-app/Cargo.toml` | Add `git_ui`, `buffer_diff`, `markdown_preview` | Modify |

---

### Task 1: Diff-watch path filter (pure logic, TDD)

**Files:**
- Create: `crates/harness-app/src/diff_watch.rs`
- Modify: `crates/harness-app/src/main.rs` (add `mod diff_watch;`) or the crate root module list (wherever modules are declared — grep `mod workspace;`)

**Interfaces:**
- Produces: `pub fn is_relevant_change(path: &std::path::Path) -> bool` — false for paths under `.git/`, `target/`, `node_modules/`; true otherwise. Later tasks call this to filter `notify` events before signalling a recompute.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::is_relevant_change;
    use std::path::Path;

    #[test]
    fn build_and_vcs_dirs_are_ignored() {
        assert!(!is_relevant_change(Path::new("/repo/.git/index")));
        assert!(!is_relevant_change(Path::new("/repo/target/debug/foo")));
        assert!(!is_relevant_change(Path::new("/repo/node_modules/x/y.js")));
        assert!(!is_relevant_change(Path::new("/repo/crates/target/x"))); // nested target
    }

    #[test]
    fn source_files_are_relevant() {
        assert!(is_relevant_change(Path::new("/repo/src/main.rs")));
        assert!(is_relevant_change(Path::new("/repo/Cargo.toml")));
        // A file literally named target (not a dir component) is still relevant.
        assert!(is_relevant_change(Path::new("/repo/src/target.rs")));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd crates/harness-app && cargo test --lib diff_watch 2>&1 | tail -20`
Expected: FAIL — `is_relevant_change` not found / module missing.

- [ ] **Step 3: Write minimal implementation**

```rust
//! Per-selection filesystem watch that drives live diff refresh (Deliverable A).
//!
//! Watches only the currently-selected worktree/main root, so cost is bounded to
//! the one diff on screen. Events are filtered here and coalesced by the workspace's
//! drain cadence (the "debounce"); the existing `diff_gen` counter drops stale work.

use std::path::Path;

/// Whether a changed path should trigger a diff recompute.
///
/// Ignores VCS metadata and build output — a `cargo build` churns `target/`
/// continuously and would otherwise storm the diff. Git already excludes ignored
/// files from the diff, so a miss here is only a wasted (deduped, debounced)
/// recompute, never a wrong diff.
pub fn is_relevant_change(path: &Path) -> bool {
    const IGNORED: [&str; 3] = [".git", "target", "node_modules"];
    !path
        .components()
        .any(|c| IGNORED.contains(&c.as_os_str().to_string_lossy().as_ref()))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd crates/harness-app && cargo test --lib diff_watch 2>&1 | tail -20`
Expected: PASS (2 tests).

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/diff_watch.rs crates/harness-app/src/main.rs
git commit -m "Diff-watch path filter: ignore .git/target/node_modules"
```

---

### Task 2: Watcher glue + workspace wiring (compile + app-verified)

**Files:**
- Modify: `crates/harness-app/src/diff_watch.rs` (add the `notify` watcher struct)
- Modify: `crates/harness-app/src/workspace.rs` (own a `DiffWatcher`, retarget in `select()` at `:1336`, drain on a timer like `:648`, call `recompute_diff`)

**Interfaces:**
- Consumes: `is_relevant_change` (Task 1); `recompute_diff` (`workspace.rs:1302`); `Resolved.path` from `resolve()` (`workspace.rs:853`).
- Produces: `DiffWatcher::new() -> (DiffWatcher, crossbeam_channel::Receiver<PathBuf>)` and `DiffWatcher::watch(&mut self, root: &Path)` (resolves+canonicalizes root, replaces any prior watch, sends the **canonical root** on the channel per relevant event).

**Design notes (read before coding):**
- Model on `crates/status-watch/src/lib.rs:181` (`notify::recommended_watcher`) and the existing channel-drain timers in `workspace.rs` (forge drain at `:648`, status at `:664`). The receiver is drained on a `cx.spawn` + `Timer` loop (~250 ms) — that cadence *is* the debounce; a burst of events collapses to one recompute per drain.
- On drain: if any message arrived whose canonical root equals the current selection's canonical root, call `recompute_diff` once (the `diff_gen` guard makes extra calls cheap/safe).
- Canonicalize the root once in `watch()`; compare canonical forms on drain (FSEvents gotcha).
- Recursive watch: `RecursiveMode::Recursive` on the root.

- [ ] **Step 1: Read the reference code**

Run: `sed -n '170,230p' crates/status-watch/src/lib.rs; sed -n '640,670p;1300,1345p' crates/harness-app/src/workspace.rs`
Note the `notify` watcher construction, the drain-timer shape, and `recompute_diff`'s signature.

- [ ] **Step 2: Add the `DiffWatcher` struct to `diff_watch.rs`**

Implement `DiffWatcher` holding the `notify::RecommendedWatcher` and the current canonical root. `new()` returns `(Self, Receiver<PathBuf>)`. The `notify` event callback filters with `is_relevant_change` and, for any relevant path, sends the current canonical root through a cloned `Sender`. `watch(root)` canonicalizes, unwatches the previous root if any, and `watch`es the new one recursively. (Follow status-watch's error handling — log and continue, never panic.)

- [ ] **Step 3: Own and drive it from the workspace**

In `HarnessWorkspace`: add a `diff_watch: DiffWatcher` field and a `diff_dirty_rx: Receiver<PathBuf>`, initialized in the constructor near the other channels (`workspace.rs:~472`). In `select()` (`:1336`), after computing the resolved path, call `self.diff_watch.watch(&resolved.path)`. Add a drain timer mirroring `:648`: on each tick, collect all pending `diff_dirty_rx` messages; if any canonical root equals the current selection's canonical path, call `self.recompute_diff(cx)`.

- [ ] **Step 4: Verify it compiles clean + clippy**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -15 && cargo clippy 2>&1 | tail -15`
Expected: builds, no new warnings.

- [ ] **Step 5: Manual app check (note in commit if deferred)**

Run the app against a repo, select a worktree, edit a file externally (`echo x >> some_file`), confirm the diff panel's numbers update within ~1s without re-clicking. If a macOS GUI run isn't available in this environment, note "runtime check deferred to user" in the commit body.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/diff_watch.rs crates/harness-app/src/workspace.rs
git commit -m "Live diff refresh: per-selection fs watcher drives recompute_diff"
```

---

### Task 3: `RepoView` git-status fields for main (TDD where possible + compile)

**Files:**
- Modify: `crates/harness-app/src/data.rs` (`RepoView` struct `:22`, `new_repo_view` `:74`)

**Interfaces:**
- Consumes: `GitWorktrees::status` (check its exact signature/return — `crates/worktrees/src/git.rs`; the report says it yields dirty count + ahead/behind) and forge `checks_for_branch` (used already in the app for worktree CI — grep `checks_for_branch` / `ci_summary`).
- Produces: `RepoView { … , dirty: usize, ahead: usize, behind: usize, ci: Option<…> }` consumed by Task 4's `render_main_entry`.

**Design notes:** Reuse `GitWorktrees::status` for main exactly as worktree cards do — do not hand-roll new git commands. CI for main is `checks_for_branch(default_branch)`; wire it through the same path `WorktreeView` uses (grep how `WorktreeView.ci` / `ci_summary` is populated and mirror it). If `status` needs a compare ref, main's is `RepoView.compare_ref`.

- [ ] **Step 1: Read the existing status/CI plumbing**

Run: `sed -n '1,120p' crates/harness-app/src/data.rs; grep -n 'fn status' crates/worktrees/src/git.rs; grep -rn 'checks_for_branch\|ci_summary\|WorktreeView' crates/harness-app/src | head -30`
Confirm the exact `status` return type and how `WorktreeView` gets its `dirty`/`ahead`/`behind`/`ci`.

- [ ] **Step 2: Add fields + population (mirror WorktreeView)**

Add `dirty`, `ahead`, `behind`, and the CI field to `RepoView` with the **same types** `WorktreeView` uses. In `new_repo_view`, populate them by calling the same `status`/CI helpers against `root` + `compare_ref`. Keep `new_repo_view` deterministic where the crate already avoids a clock.

- [ ] **Step 3: Test (if `worktrees` has a real-git test harness, add one; else rely on compile)**

If `crates/worktrees/tests` provides a temp-repo fixture, add a test asserting `status` returns a nonzero dirty count after writing an untracked file (this pins the reuse). Otherwise, this is compile-verified and exercised by Task 4's render.

Run: `cargo test -p worktrees 2>&1 | tail -15`
Expected: existing 8+ tests still pass (plus any added).

- [ ] **Step 4: Verify compile**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -15`
Expected: builds (Task 4 consumes the new fields; a temporary `#[allow(dead_code)]` is fine until then, removed in Task 4).

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/data.rs crates/worktrees
git commit -m "RepoView carries dirty/ahead/behind/CI for the main row"
```

---

### Task 4: Main sidebar parity — live pill, accent border, badges, status watch (compile + probe)

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` — `render_main_entry` (`:2745`), status-watch registration (`:474-478` and in `refresh` `:1368-1379`)

**Interfaces:**
- Consumes: `RepoView` fields (Task 3); `agent_pill(hx, state)` (`workspace.rs`, used at `:2844`); `self.agent_status.get(&repo_root)`; the badge render helpers used by `render_card` (`:2847-2887`); `StatusFileSource::watch` (`:474`).

**Design notes:** Copy the *shape* of `render_card` (`:2792-2929`) into `render_main_entry`, dropping the PR badge, the editable name/avatar, and the right-click menu. Selection highlight: replace the bg-only branch (`:2763`) with `render_card`'s accent-border branch (`:2898`). Pill: replace the static `✓` (`:2789`) with `agent_pill(hx, self.agent_status.get(&repo.repo_root).copied().unwrap_or_default())`.

- [ ] **Step 1: Register the repo root with the status watcher**

At the two watch-registration sites (`:474-478` startup, `:1368-1379` refresh), add each `repo.repo_root` to the set handed to `StatusFileSource::watch` alongside the worktree paths. Read those sites first to match the exact call shape.

- [ ] **Step 2: Render the live pill + accent border + badges**

Edit `render_main_entry`: swap the static check for `agent_pill(...)`; give the row the accent left-border when `matches!(self.selection, Selection::Main(ref n) if n == &repo.name)`; add the dirty/ahead/behind/CI badges using the same helpers `render_card` uses (no PR badge).

- [ ] **Step 3: Remove any `#[allow(dead_code)]` added in Task 3**

The `RepoView` fields are now consumed.

- [ ] **Step 4: Verify compile + clippy**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -15 && cargo clippy 2>&1 | tail -15`
Expected: clean.

- [ ] **Step 5: Runtime check**

In the app: run an agent on main (`+` → an agent on the main row), confirm the pill goes RUNNING/WAITING/IDLE and a notification fires; confirm the accent border shows when main is selected and the dirty/ahead/behind/CI badges render. Defer to user if no GUI here (note in commit).

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/workspace.rs
git commit -m "First-class main: live status pill, accent selection, badges, status watch"
```

---

### Task 5: B1 — ⌘P / ⌘⇧F focus fix (probe-verified)

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` — `focus_active_pane` (`:1104`) / Editor tab activation, and the `TabContent::Editor` render arm (`:3079`)

**Design notes:** The bindings are `Scope::Workspace` (`shortcuts.rs:167-176`); they only match when focus is inside the hosted-workspace subtree (context supplied at `:3092`). Ensure that when the **Editor** tab becomes active, focus is placed on `ws.read(cx).focus_handle(cx)` (the active pane's handle, which carries `Workspace > Pane > EmptyPane` even with no file). Only nudge focus when the active tab is Editor — never steal from terminal/chat.

- [ ] **Step 1: Read the focus + probe code**

Run: `sed -n '1100,1120p;3075,3110p' crates/harness-app/src/workspace.rs; sed -n '540,710p' crates/harness-app/src/workspace_host.rs`
Understand `focus_active_pane` and the `HARNESS_PROBE_KEYS`/`probe_finder` harness.

- [ ] **Step 2: Place Workspace focus on Editor-tab activation**

In the tab-activation path, when the newly-active tab is `TabContent::Editor`, call `ws.update(cx, |ws, cx| ws.focus_handle(cx).focus(window))` (match the exact focus API the probes use). Guard so it fires only for the Editor tab.

- [ ] **Step 3: Probe — actions fire with no file open**

Run the existing probe that dispatches ⌘P/⌘⇧F through the real keymap (per `workspace_host.rs` `probe_finder`/`probe_keys`; invoked via an env var such as `HARNESS_PROBE_KEYS=1`). Assert the file-finder modal opens and a search item lands **without opening a file first**. Reproduce the exact env/command from the probe source.

Expected: probe reports the finder opened / search deployed.

- [ ] **Step 4: Verify compile + clippy**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -15 && cargo clippy 2>&1 | tail -15`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/workspace.rs
git commit -m "Editor: ⌘P/⌘⇧F work whenever the Editor tab is active (focus fix)"
```

---

### Task 6: B2 — native git/diff via `git_ui` (compile + patch re-diff)

**Files:**
- Modify: `crates/harness-app/Cargo.toml` (add `git_ui`, `buffer_diff`)
- Modify: `crates/harness-app/src/main.rs` (call `git_ui::init(cx)` near `:159-231`)
- Modify: `crates/harness-app/src/shortcuts.rs` (bind the project-diff action, `Scope::Workspace`)

**Design notes:** `git_ui::init` registers via `cx.observe_new::<Workspace>` (zed `git_ui.rs:82`), auto-attaching to the hosted Workspace. The project-diff opens as a **pane item** — no modal host needed. Gutter change-bars come from the editor reading `BufferDiff` off the hosted `Project`'s git store.

- [ ] **Step 1: Add the crates**

In `harness-app/Cargo.toml`, add `git_ui` and `buffer_diff` as git deps at rev `952d712` (copy the exact `git`/`rev` form the existing zed deps use).

- [ ] **Step 2: Re-diff the `[patch.crates-io]` set (R1a)**

Run: `git -C ~/.cargo/git/checkouts/zed-*/952d712 show HEAD:Cargo.toml 2>/dev/null | sed -n '/\[patch.crates-io\]/,/^\[/p'` (or locate zed's root Cargo.toml in the checkout) and compare to `harness-app/Cargo.toml`'s `[patch.crates-io]`. Add any missing entries `git_ui` needs.

- [ ] **Step 3: Init in `main.rs`**

Add `git_ui::init(cx);` alongside the other `::init` calls (order after `workspace::init`, before/after `project_panel::init` — match zed's own ordering in its `main.rs` if unsure).

- [ ] **Step 4: Bind the action**

In `shortcuts.rs`, add a `Scope::Workspace` binding for the project-diff action (`git::Diff` / `OpenModifiedFiles` — confirm the action path from `git_ui/src/git_ui.rs:296`). Pick a chord that doesn't collide (grep existing chords).

- [ ] **Step 5: Build (cold — expect it long) + clippy**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -25 && cargo clippy 2>&1 | tail -15`
Expected: compiles; if a `[patch]` entry is missing you'll see a version-mismatch error → fix Step 2.

- [ ] **Step 6: Runtime check + commit**

In the app: trigger the project-diff action → the multi-file diff opens as an editor pane; open a modified file → gutter change-bars show. Defer to user if no GUI (note in commit).

```bash
git add crates/harness-app/Cargo.toml crates/harness-app/Cargo.lock crates/harness-app/src/main.rs crates/harness-app/src/shortcuts.rs
git commit -m "Editor: native Zed git/diff via git_ui (project diff + gutter change-bars)"
```

---

### Task 7: B3 — native Markdown preview via `markdown_preview` (compile)

**Files:**
- Modify: `crates/harness-app/Cargo.toml` (add `markdown_preview`)
- Modify: `crates/harness-app/src/main.rs` (`markdown_preview::init(cx)`)
- Modify: `crates/harness-app/src/shortcuts.rs` (bind `OpenPreview` + `OpenPreviewToTheSide`)

**Design notes:** `markdown_preview::init` registers via `cx.observe_new::<Workspace>` (zed `markdown_preview.rs:37`); the preview opens as a pane item tracking the active buffer. The custom `markdown_preview.rs` diff-panel `◧` toggle stays untouched.

- [ ] **Step 1: Add the crate + re-diff patches**

Add `markdown_preview` (rev `952d712`) to `harness-app/Cargo.toml`; repeat the `[patch]` re-diff (Task 6 Step 2) for anything it introduces.

- [ ] **Step 2: Init**

Add `markdown_preview::init(cx);` beside the other inits in `main.rs`.

- [ ] **Step 3: Bind the actions**

In `shortcuts.rs`, bind `zed_actions::preview::markdown::OpenPreview` and `OpenPreviewToTheSide` at `Scope::Workspace` (confirm the action paths from zed `markdown_preview_view.rs:97-113`). Non-colliding chords.

- [ ] **Step 4: Build + clippy**

Run: `cd crates/harness-app && cargo build 2>&1 | tail -25 && cargo clippy 2>&1 | tail -15`
Expected: clean.

- [ ] **Step 5: Runtime check + commit**

In the app: open a `.md` file, trigger OpenPreview → a live rendered preview opens as a pane with highlighted fenced code. Defer to user if no GUI (note in commit).

```bash
git add crates/harness-app/Cargo.toml crates/harness-app/Cargo.lock crates/harness-app/src/main.rs crates/harness-app/src/shortcuts.rs
git commit -m "Editor: native Zed Markdown preview (live, code-highlighted)"
```

---

## Self-review

- **Spec coverage:** A→Tasks 1-2; C→Tasks 3-4; B1→Task 5; B2→Task 6; B3→Task 7. All four deliverables covered.
- **Placeholders:** pure-logic code is concrete (Task 1). GPUI wiring tasks give exact files+line anchors and a "read first" step because exact signatures at rev `952d712` must be read from the vendored source, not guessed — fabricating them would be the worse failure. Each such task is compile- and probe-gated.
- **Type consistency:** `is_relevant_change`, `DiffWatcher::new/watch`, `RepoView` field names (`dirty`/`ahead`/`behind`/`ci`), and `agent_pill` usage are consistent across tasks.
- **Ordering:** A and C independent; B1 before B2/B3 (focus gate). Matches the spec's `A → C → B1 → B2 → B3`.
