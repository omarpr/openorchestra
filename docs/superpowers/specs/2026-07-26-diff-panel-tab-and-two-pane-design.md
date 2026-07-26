# Diff panel: tab location, two-pane layout, and edit intent

**Date:** 2026-07-26
**Status:** approved

## Problem

The diff panel is a fixed right dock. Three things follow from that, and each is
a separate complaint:

1. It is *only* a dock. On a laptop it eats horizontal space that the editor and
   terminal want, and there is no way to give the diff the whole window.
2. It renders as an accordion — one scrolling column of file rows, each
   expanding its unified diff inline beneath itself. Reviewing means scrolling
   past the diff you just read to reach the next filename. Zed and GitHub both
   put files on the left and the selected file's changes on the right, which
   keeps the file list stationary while you read.
3. The diff rows are read-only text with no exit. Noticing something wrong while
   reading a diff is the single most common reason to want an editor, and today
   that means finding the file yourself in a separate editor tab.

A fourth, unrelated defect surfaced while reading the code and is fixed here
because it lives in the same neighbourhood: the Markdown-preview eye button
renders on the far *left* of the editor toolbar instead of the right.

## Design

### 1. Location: a setting, not a second surface

New pref:

```rust
enum DiffLocation { Dock, Tab }   // serde default: Dock
```

`Dock` is the default, so an existing `prefs.json` — which has no
`diff_location` key — keeps today's behaviour exactly.

- **Dock mode** — the root layout is unchanged (`workspace.rs:976`).
- **Tab mode** — the dock never renders. The top-bar diff button
  (`ui/chrome.rs:43`) focuses the active worktree's Diff tab instead, opening
  one if there isn't one. `diff_open` stays as the dock's visibility flag and is
  simply not consulted in Tab mode.

`TabKind::Diff` and `TabContent::Diff` are added. **`TabContent::Diff` is a unit
variant**, unlike the other three: the diff state (`self.diff`, `self.review`,
`self.expanded`, `self.diff_selection`) already lives on `HarnessWorkspace` and
is already resolved per worktree. The tab is a marker telling `render_center` to
draw the diff surface. Giving it an entity would create a second copy of state
that could disagree with the dock's.

`panes_state::TabRecordKind::Diff` persists it, so a Diff tab survives restart
the way editor and chat tabs do. The `＋` menu gains a `⑂ Diff` item, shown only
in Tab mode — in Dock mode the dock *is* the surface, and offering both would
reintroduce the two-surfaces problem the setting exists to avoid. For the same
reason, switching Tab→Dock closes any open Diff tabs rather than leaving a tab
rendering a surface that has moved.

### 2. Two-pane layout

`render_diff_panel` splits into a shared `render_diff_surface`, which dispatches
to `render_file_list` + `render_file_diff` (two-pane) or the existing accordion
(stacked). The choice is deterministic and needs no measurement:

- **Tab mode** → always two-pane.
- **Dock mode** → two-pane iff `diff_width >= DIFF_TWO_PANE_MIN` (560), else the
  accordion.

Extracted as a pure `use_two_pane(location, width) -> bool` so it is testable
without a window.

**Selection state.** Two-pane tracks `diff_focus: Option<String>` — the last
file clicked, defaulting to the first changed file, re-validated against
`self.diff.files` on every refresh so a file that stops changing cannot leave
the right pane pointing at nothing. The accordion keeps `expanded: HashSet`
untouched. Two fields with one job each, and no reconciliation when the layout
flips; the alternative (one shared selection) would have cost the accordion its
multi-expand, which is what makes a narrow dock scannable.

The left column is resizable through a third `Drag::DiffList` target reusing the
existing splitter infrastructure, persisted as `diff_list_width` (default 280,
clamped 160–560).

Header, `+`/`−` totals, the review bar, reviewed checkboxes, the
Markdown-preview toggle, and inline comments all carry over unchanged; in
two-pane the preview and the diff body render in the right pane.

One fix falls out of the move: the comment popover hard-codes opening leftward
(`ui/center.rs:247`) because the dock is on the right. In Tab mode that pushes
it off the wrong edge, so it becomes side-aware — it opens left of the click
when the click is in the right half of the window, right of it otherwise.

### 3. Edit intent

**Trigger:** a double-click on a diff row (`MouseDownEvent::click_count >= 2`,
folded into the existing row handler at `ui/diff.rs:242`), or a printable
keystroke while a row is selected. Single-click still selects rows for comments,
so nothing existing changes.

**Line targeting.** `readable_diff_lines` gives `Removed` rows their *old*-file
line number (`workspace.rs:165`), which does not exist in the working file —
jumping to it would land on the wrong line, silently. A pure
`target_line_for_row(rows, ix) -> Option<u32>` therefore walks forward from a
removed row to the next Added/Context row and uses its new-file number; Added
and Context rows use their own. A run of removed rows at the end of a file has
no following row and yields `None`, which opens the file without a jump.

**Flow:**

```
request_edit(file, line):
  editors = this worktree's tabs holding TabContent::Editor
  if empty  -> open_tab(Editor), then open the file at line   // no question
  else      -> ask
```

The question is a small centred overlay following the `ClosePrompt` pattern
(`workspace.rs:1114`): **Open in editor** (targeting the most recently active
editor tab) · **New editor tab** · esc or backdrop-click to cancel. Two real
choices plus cancel — with several editor tabs open it still targets one, rather
than growing a button per tab.

Opening reuses the recipe already proven in `probe_open`
(`workspace_host.rs:552`) — `RelPath` → `ProjectPath` → `open_path` →
`change_selections` to the line → focus — extracted as
`workspace_host::open_file_at`. The harness tab then switches to that editor
tab, because opening a file in a tab the user cannot see is indistinguishable
from doing nothing.

A file with `Change::Deleted` has nothing to open: it sets `self.note` and never
prompts.

### 4. The Markdown-preview eye

Zed's `Toolbar::render` right-aligns the `PrimaryRight` group with
`justify_between`, which only pushes right when a `PrimaryLeft` item exists
(`toolbar.rs:130–156`). Zed always has one — Breadcrumbs — and this embed
installs none, so the group collapses to the left edge and takes the eye with
it. The button is already `PrimaryRight` and already added last, which in the
group's `flex_row_reverse` is the leftmost slot, so its ordering is already
Zed's.

Fix: install zed's `breadcrumbs` toolbar item as `PrimaryLeft` in `init_toolbar`
(`workspace_host.rs:208`), matching zed's own pane init. The eye then aligns
right structurally rather than by compensating for a missing sibling, and the
editor gains zed's real breadcrumb trail.

## Testing

The rendering is GPUI and not unit-testable in this crate, so every decision the
UI makes is factored into a pure function and tested there:

- `target_line_for_row` — removed rows map forward to the next new-file line;
  added/context map to their own; a trailing run of removed rows yields `None`.
- `use_two_pane(location, width)` — Tab always true; Dock at the threshold.
- `Prefs` round-trip — a `prefs.json` with no `diff_location` loads as `Dock`.
- `diff_list_width` clamping in `Prefs::sanitized`.
- `panes_state` round-trip carrying a `TabRecordKind::Diff` record.

Visual behaviour is verified by running the app. `harness-app` is built from
`crates/harness-app` directly — it is its own workspace root and the repo-root
cargo workspace skips it.

## Files

- `workspace.rs` — `Prefs` (`diff_location`, `diff_list_width`), `TabKind::Diff`,
  `TabContent::Diff`, `Drag::DiffList`, `diff_focus`, `edit_prompt`, root gating.
- `workspace/state.rs` — `open_tab` Diff arm, `restore_tabs` Diff arm,
  `request_edit`, target-editor resolution.
- `workspace/ui/diff.rs` — surface split, two-pane renderers, double-click.
- `workspace/ui/diff_edit.rs` *(new)* — the edit prompt. `diff.rs` is ~600 lines
  and grows here; the prompt is a separable concern.
- `workspace/ui/center.rs` — Diff tab content, `＋` menu item, popover side.
- `workspace/ui/settings.rs` — the location chips.
- `workspace/ui/chrome.rs` — the top-bar button's Tab-mode behaviour.
- `workspace_host.rs` — `open_file_at`, breadcrumbs toolbar item.
- `panes_state.rs` — `TabRecordKind::Diff`.
- `Cargo.toml` — `breadcrumbs` dependency.
