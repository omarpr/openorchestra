# Diff-base dropdown

**Date:** 2026-07-28
**Status:** approved (design)
**Supersedes:** architecture decision **D10**

## Problem

The diff panel always compares merge-base → working tree, so a task's own
commits appear alongside its uncommitted edits. Reading the panel, you cannot
tell which is which, and the common question — *what have I changed but not
committed?* — has no answer in the UI.

The main entry does answer it, by accident of configuration: `resolve()` hands
it `compare_ref: "HEAD"`, so its merge-base collapses to `HEAD` and the same
code path yields a working-tree-only diff. Two selections, same code, opposite
meaning, nothing on screen naming which one you are looking at.

## Decision

Make the base an explicit, user-visible choice with two values, defaulting to
uncommitted work.

| Option | Base ref | Answers |
|---|---|---|
| `Uncommitted` (default) | `HEAD` | What have I changed and not committed? |
| `vs <default branch>` | repo's `compare_ref` | What does this whole branch change? (today's behaviour, the PR view) |

There is deliberately **no third "parent branch" option**. Nothing records the
branch a worktree was created from — `WorktreeEntry` stores `branch` and no
base, and git does not retain the start-point of `worktree add -b` either. In
this workspace every task branches from the default branch, so a "parent" row
would duplicate row 2 while implying a precision the data cannot support. If
tasks ever branch from each other, record a `base_branch` on `WorktreeEntry` at
creation and add the row then.

## Design

### State

```rust
enum DiffBase { Uncommitted, DefaultBranch }
```

Stored on `Prefs` as `diff_base` with `#[serde(default)]`, whose `Default` is
`Uncommitted`. Existing `prefs.json` files therefore land on the new default
with no migration step.

The setting is **app-wide and persisted**: switching it holds across task
selection and across restarts. One field of state, no per-task map to explain
or invalidate when a task is deleted.

### Resolution

One pure function in `workspace/state.rs`, beside `resolve()`, which is where
`compare_ref` is decided today — the whole of the behaviour change:

```rust
fn diff_base_ref(base: DiffBase, compare_ref: &str) -> &str
```

`Uncommitted` → `"HEAD"`; `DefaultBranch` → `compare_ref`. Passing `HEAD` makes
`GitWorktrees::diff`'s `merge-base HEAD HEAD` collapse to `HEAD`, so `git diff
HEAD` reports the working tree. This is not a new code path — it is the one the
main entry has always used, now reachable deliberately.

Applied at **both** compute sites, or a multi-repo task would ignore the
setting:

- `state.rs:1460` — single member
- `state.rs:1413` — multi-member aggregate, where each member maps its own
  `compare_ref`

### What deliberately does not change

The sidebar's ahead/behind/dirty badges read `repo.compare_ref` directly
(`state.rs:1143`), never `Resolved`. *How far ahead is this branch* is a
different question from *what am I reading in the diff panel*, and a viewer
toggle must not silently redefine it. Same for `push_state` and the push
button.

### Untracked files

`git diff` never lists untracked files. Since `Uncommitted` becomes the default
view for every task, a file you just created would otherwise be invisible until
staged — the default view would misreport the working tree.

Untracked files are therefore merged into the file list as `Change::Added`, in
**both** bases. The endpoint of both ranges is the working tree, and an
untracked file is part of it; including it in one view and not the other would
reintroduce the confusion this feature exists to remove. *(This extends the
approved decision, which covered only the `Uncommitted` view — flagged here for
review.)*

In `worktrees`:

```rust
pub fn untracked_files(&self, worktree: &Path) -> Vec<PathBuf>
pub fn diff_including_untracked(&self, worktree: &Path, base_ref: &str) -> Result<Diff, GitError>
```

- Discovery: `git ls-files --others --exclude-standard`, so `.gitignore` is
  respected.
- Content: `git diff --no-index -- /dev/null <path>` per file, which yields a
  proper unified hunk and handles binaries ("Binary files differ") without
  special-casing.
- The index is never touched. `git add -N` would produce this for free and is
  what some tools do; a viewer that stages your files as a side effect of being
  looked at is not acceptable.
- Capped at `MAX_UNTRACKED_FILES` (100), sorted by path for a stable list. When
  the cap elides files the panel says how many, per the existing "never
  truncate silently" convention (`ui/diff.rs:329`).

### UI

A button in the diff header, **before** "Changed files", showing the live base:
`Uncommitted` or `vs origin/master`. Clicking opens the existing `CardMenu`
popover pattern (absolute position, backdrop dismiss on mouse-down) with the
two rows and a check on the active one.

The label names the resolved ref rather than a fixed string, so it degrades
honestly to `vs master` on a repo with no remote.

Switching writes the pref, saves, and calls `recompute_diff`, which bumps
`diff_gen` — the existing guard that discards an in-flight result for a base
the user has left.

### Testing

Pure and unit-testable, no GPUI harness:

- `diff_base_ref` maps both variants, including a `compare_ref` that is already
  `HEAD`.
- `Prefs` round-trip: a JSON document with no `diff_base` key deserializes to
  `Uncommitted`; a written pref reloads.
- Label formatting with a remote (`vs origin/master`) and without (`vs master`).
- `untracked_files` respects `.gitignore` and returns paths sorted — real-git
  test, following the existing `crates/worktrees` real-git test style.
- `diff_including_untracked` lists a new file as `Change::Added` with its line
  count, and leaves the index clean afterwards (`git status --porcelain`
  unchanged).
- Cap: more than `MAX_UNTRACKED_FILES` untracked files elides and reports.

## Consequences

- **D10 is reversed** and `docs/architecture.md` must be rewritten to describe
  the toggle and its default, not the old fixed merge-base.
- The default view changes for every existing user on upgrade. This is the
  intent — the old default is one click away and the label now says which is
  active.
- One extra git call per diff (`ls-files`), plus one per untracked file up to
  the cap, on the existing background thread.
