# Diff Panel: Tab Location, Two-Pane Layout, Edit Intent — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the diff panel live as a center tab instead of a right dock, render it Zed-style (files left / changes right), and offer to open a file in an editor when the user tries to edit a diff line.

**Architecture:** The diff surface stays a set of `impl HarnessWorkspace` render methods reading state that already lives on the view — the new Diff tab is a *unit* `TabContent` variant, so there is exactly one copy of diff state regardless of where it is drawn. Every decision the renderer makes (which layout, which line to jump to) is factored into a pure function that is unit-tested without a window, since GPUI rendering is not testable in this crate.

**Tech Stack:** Rust 2024, GPUI + zed crates pinned at rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4`, serde/serde_json for persisted prefs.

## Global Constraints

- `harness-app` is **its own workspace root** and is excluded from the repo-root cargo workspace. Build and test it from `crates/harness-app`; `cargo test` at the repo root silently skips it and reports a false green.
- Test command for every task: `cd crates/harness-app && cargo test`. Compile check: `cd crates/harness-app && cargo check`.
- All zed crates stay on rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4` (docs/architecture.md §3.1). Any new zed dependency uses that exact rev.
- New `Prefs` fields carry `#[serde(default)]` and a `Default` value equal to today's behaviour, so an existing `~/.openorchestra/prefs.json` keeps working unchanged.
- The crate has its own `markdown_preview` module shadowing zed's crate of the same name — reference zed's as `::markdown_preview`.
- Files in `workspace/ui/` are `impl HarnessWorkspace` blocks opened with `use super::super::*;`. New UI files follow that shape and get a `mod` line in `workspace/ui.rs`.
- Commit after each task.

---

### Task 1: Prefs — diff location and file-list width

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (`Prefs` struct ~1320, `Default` ~1370, `sanitized` ~1394, tests ~1740)

**Interfaces:**
- Produces: `enum DiffLocation { Dock, Tab }` (derives `Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize`, `#[serde(rename_all = "lowercase")]`, `Dock` is `#[default]`); `Prefs::diff_location: DiffLocation`; `Prefs::diff_list_width: f32`.

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` in `workspace.rs`:

```rust
#[test]
fn a_prefs_file_without_a_diff_location_loads_as_the_dock() {
    // The setting is new; every existing install must keep the dock it has.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefs.json");
    std::fs::write(
        &path,
        r#"{"dark":true,"sidebar_width":280.0,"diff_width":340.0,"sidebar_open":true,"diff_open":true}"#,
    )
    .unwrap();
    let prefs = load_prefs(&path);
    assert_eq!(prefs.diff_location, DiffLocation::Dock);
    assert_eq!(prefs.diff_list_width, 280.0);
}

#[test]
fn the_diff_list_width_is_clamped_like_every_other_panel() {
    // A hand-edited or truncated file must not render the list unusable.
    let squashed = Prefs { diff_list_width: -10.0, ..Prefs::default() }.sanitized();
    assert!(squashed.diff_list_width >= 160.0);
    let huge = Prefs { diff_list_width: 9999.0, ..Prefs::default() }.sanitized();
    assert!(huge.diff_list_width <= 560.0);
}

#[test]
fn the_diff_location_survives_a_save_load_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefs.json");
    let prefs = Prefs { diff_location: DiffLocation::Tab, ..Prefs::default() };
    save_prefs_to(&path, &prefs);
    assert_eq!(load_prefs(&path).diff_location, DiffLocation::Tab);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd crates/harness-app && cargo test a_prefs_file_without_a_diff_location`
Expected: FAIL — `cannot find type DiffLocation in this scope`.

- [ ] **Step 3: Add the enum and the fields**

Above `struct Prefs` in `workspace.rs`:

```rust
/// Where the diff surface is drawn. `Dock` is the right-hand panel this app
/// shipped with; `Tab` moves it into the center tab strip, where it gets the
/// whole window and the dock is not rendered at all.
///
/// One location, not two: a docked panel and a tab showing the same review
/// would each need their own scroll and selection, and the two would disagree
/// the moment either was touched.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffLocation {
    #[default]
    Dock,
    Tab,
}
```

In `struct Prefs`, after `diff_width`:

```rust
    /// Where the diff surface lives (dock vs. center tab).
    #[serde(default)]
    diff_location: DiffLocation,
    /// Width of the file-list column in the two-pane diff layout.
    #[serde(default = "default_diff_list_width")]
    diff_list_width: f32,
```

Next to `default_true`:

```rust
fn default_diff_list_width() -> f32 {
    280.0
}
```

In `impl Default for Prefs`, after `diff_width: 340.0,`:

```rust
            diff_location: DiffLocation::Dock,
            diff_list_width: 280.0,
```

In `Prefs::sanitized`, after the `diff_width` clamp:

```rust
        self.diff_list_width = self.diff_list_width.clamp(160.0, 560.0);
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd crates/harness-app && cargo test prefs`
Expected: PASS, including the pre-existing `prefs_*` tests.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/workspace.rs
git commit -m "feat(prefs): diff location and file-list width"
```

---

### Task 2: Pure layout and line-targeting helpers

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (free functions near `readable_diff_lines` ~91; tests in the existing `mod tests`)

**Interfaces:**
- Consumes: `DiffLocation` (Task 1); the existing `DiffRow { kind, number, text }` and `DiffLineKind { FileHeader, HunkBreak, Added, Removed, Context }`.
- Produces: `fn use_two_pane(location: DiffLocation, dock_width: f32) -> bool`; `fn target_line_for_row(rows: &[DiffRow], ix: usize) -> Option<u32>`; `const DIFF_TWO_PANE_MIN: f32`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn the_tab_is_always_two_pane_and_the_dock_only_when_wide_enough() {
    // A tab fills the center, so it always has room; the dock can be dragged
    // down to 220px, where two columns would leave neither readable.
    assert!(use_two_pane(DiffLocation::Tab, 0.0));
    assert!(!use_two_pane(DiffLocation::Dock, 340.0));
    assert!(!use_two_pane(DiffLocation::Dock, DIFF_TWO_PANE_MIN - 1.0));
    assert!(use_two_pane(DiffLocation::Dock, DIFF_TWO_PANE_MIN));
    assert!(use_two_pane(DiffLocation::Dock, 800.0));
}

#[test]
fn an_added_or_context_row_targets_its_own_line() {
    let rows = readable_diff_lines(
        "diff --git a/x.rs b/x.rs\n@@ -1,2 +1,3 @@\n ctx\n+added\n ctx2\n",
    );
    // Row 0 is the file header; find the content rows by kind.
    let added = rows.iter().position(|r| r.kind == DiffLineKind::Added).unwrap();
    assert_eq!(target_line_for_row(&rows, added), rows[added].number);
    let ctx = rows.iter().position(|r| r.kind == DiffLineKind::Context).unwrap();
    assert_eq!(target_line_for_row(&rows, ctx), rows[ctx].number);
}

#[test]
fn a_removed_row_targets_the_next_line_that_still_exists() {
    // A removed row carries its OLD file line number, which does not exist in
    // the working file. Jumping to it would land on an unrelated line and look
    // like the feature simply picked the wrong place.
    let rows = readable_diff_lines(
        "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,2 @@\n keep\n-gone\n after\n",
    );
    let removed = rows.iter().position(|r| r.kind == DiffLineKind::Removed).unwrap();
    let after = rows
        .iter()
        .enumerate()
        .find(|(i, r)| *i > removed && r.kind == DiffLineKind::Context)
        .map(|(_, r)| r.number)
        .unwrap();
    assert_eq!(target_line_for_row(&rows, removed), after);
}

#[test]
fn a_trailing_run_of_removed_rows_targets_nothing() {
    // Deleting the tail of a file leaves no following line to jump to. Opening
    // the file with no jump is right; guessing a line is not.
    let rows = readable_diff_lines(
        "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,1 @@\n keep\n-gone\n-also gone\n",
    );
    let removed = rows.iter().position(|r| r.kind == DiffLineKind::Removed).unwrap();
    assert_eq!(target_line_for_row(&rows, removed), None);
}

#[test]
fn a_header_or_hunk_break_targets_nothing_and_an_out_of_range_index_is_safe() {
    let rows = readable_diff_lines("diff --git a/x.rs b/x.rs\n@@ -1,1 +1,1 @@\n keep\n");
    assert_eq!(target_line_for_row(&rows, 0), None, "file header");
    assert_eq!(target_line_for_row(&rows, 9999), None, "out of range");
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd crates/harness-app && cargo test two_pane`
Expected: FAIL — `cannot find function use_two_pane in this scope`.

- [ ] **Step 3: Write the implementations**

Below `readable_diff_lines` in `workspace.rs`:

```rust
/// Dock width at or above which the diff surface splits into two columns.
///
/// Below this a file list and a diff column would each be too narrow to read a
/// path or a line of code without truncation, so the accordion — which gives
/// the full width to whichever file is open — stays the better layout.
const DIFF_TWO_PANE_MIN: f32 = 560.0;

/// Whether to draw the diff surface as files-left/changes-right.
///
/// Decided from the location and the persisted dock width rather than measured
/// bounds: a tab always fills the center, and the dock's width is a pref we
/// already own, so neither case needs a layout pass to answer.
fn use_two_pane(location: DiffLocation, dock_width: f32) -> bool {
    match location {
        DiffLocation::Tab => true,
        DiffLocation::Dock => dock_width >= DIFF_TWO_PANE_MIN,
    }
}

/// The line in the *working file* that diff row `ix` refers to.
///
/// Added and context rows already carry new-file numbers. A removed row does
/// not: `readable_diff_lines` gives it the OLD file's number, which addresses a
/// line that is gone. Opening the file there would land somewhere unrelated and
/// look like a targeting bug rather than a missing line, so a removed row
/// resolves to the next row that still exists. A removed run at the end of a
/// file has no successor and yields `None` — open the file, don't guess.
fn target_line_for_row(rows: &[DiffRow], ix: usize) -> Option<u32> {
    let row = rows.get(ix)?;
    match row.kind {
        DiffLineKind::Added | DiffLineKind::Context => row.number,
        DiffLineKind::Removed => rows[ix + 1..]
            .iter()
            .take_while(|r| r.kind != DiffLineKind::FileHeader)
            .find(|r| matches!(r.kind, DiffLineKind::Added | DiffLineKind::Context))
            .and_then(|r| r.number),
        DiffLineKind::FileHeader | DiffLineKind::HunkBreak => None,
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd crates/harness-app && cargo test two_pane && cd crates/harness-app && cargo test target_line`
Expected: PASS (5 new tests).

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/workspace.rs
git commit -m "feat(diff): pure two-pane and line-targeting helpers"
```

---

### Task 3: The Diff tab — type, persistence, open, restore

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (`TabContent` ~194, `TabKind` ~205, `Tab::kind_tag` ~230)
- Modify: `crates/harness-app/src/panes_state.rs` (`TabRecordKind` ~37, tests ~66)
- Modify: `crates/harness-app/src/workspace/state.rs` (`open_tab` ~480, `restore_tabs` ~953)

**Interfaces:**
- Produces: `TabKind::Diff`, `TabContent::Diff`, `panes_state::TabRecordKind::Diff`. `open_tab(TabKind::Diff, ..)` creates a tab titled `"⑂ diff"`.

- [ ] **Step 1: Write the failing test**

In `panes_state.rs` `mod tests`:

```rust
#[test]
fn a_diff_tab_round_trips_through_disk() {
    // The diff tab is a view over state the app already holds, but WHICH tabs
    // are open is persisted like any other, so reopening restores the layout.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("panes.json");
    let mut state = PanesState { next_tab_id: 2, worktrees: BTreeMap::new() };
    state.worktrees.insert(
        "acme/fix".into(),
        WorktreePanes {
            active: Some(1),
            tabs: vec![TabRecord { id: 1, kind: TabRecordKind::Diff, title: "⑂ diff".into() }],
        },
    );
    save(&path, &state);
    assert_eq!(load(&path), state);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd crates/harness-app && cargo test a_diff_tab_round_trips`
Expected: FAIL — `no variant named Diff found for enum TabRecordKind`.

- [ ] **Step 3: Add the variants and the arms**

`panes_state.rs`, in `enum TabRecordKind`:

```rust
    Diff,
```

`workspace.rs`, in `enum TabContent`:

```rust
    /// The diff review surface, when `DiffLocation::Tab` puts it in the center.
    ///
    /// A UNIT variant, unlike the other three: the diff, the review notes, and
    /// the expand/selection state all live on `HarnessWorkspace` already and
    /// are resolved per worktree. Giving the tab its own entity would create a
    /// second copy that could disagree with the dock's.
    Diff,
```

`workspace.rs`, in `enum TabKind`:

```rust
    Diff,
```

`workspace.rs`, in `Tab::kind_tag`'s match:

```rust
            TabContent::Diff => K::Diff,
```

`workspace/state.rs`, in `open_tab`'s `match kind`:

```rust
            TabKind::Diff => (TabContent::Diff, "⑂ diff".to_string()),
```

`workspace/state.rs`, in `restore_tabs`'s `match kind`:

```rust
                K::Diff => self.open_tab(TabKind::Diff, window, cx),
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd crates/harness-app && cargo test`
Expected: PASS. `cargo check` will still flag non-exhaustive matches in render code — that is Task 4's job; if `cargo test` cannot build because of them, add `TabContent::Diff => div().into_any_element(),` to the `render_center` match now and replace it in Task 4.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/src/workspace.rs crates/harness-app/src/panes_state.rs crates/harness-app/src/workspace/state.rs
git commit -m "feat(diff): a Diff tab kind that persists like the others"
```

---

### Task 4: Tab mode wiring — root layout, top bar, ＋ menu, settings

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (root render ~976)
- Modify: `crates/harness-app/src/workspace/ui/center.rs` (`render_center` ~24, `render_tab_menu` ~386)
- Modify: `crates/harness-app/src/workspace/ui/chrome.rs` (`diff_toggle` ~30)
- Modify: `crates/harness-app/src/workspace/ui/settings.rs` (Extras section ~279)
- Modify: `crates/harness-app/src/workspace/state.rs` (new methods)

**Interfaces:**
- Consumes: `DiffLocation` (Task 1), `TabKind::Diff` / `TabContent::Diff` (Task 3), `render_diff_surface` (Task 5 — until then call the existing `render_diff_panel`).
- Produces: `HarnessWorkspace::toggle_diff(&mut self, window, cx)`; `HarnessWorkspace::set_diff_location(&mut self, DiffLocation, window, cx)`; `HarnessWorkspace::diff_tab_id(&self) -> Option<u64>`.

- [ ] **Step 1: Add the state methods**

In `workspace/state.rs`, in the `impl HarnessWorkspace` block:

```rust
    /// The active worktree's Diff tab, if it has one.
    pub(crate) fn diff_tab_id(&self) -> Option<u64> {
        let r = self.resolve()?;
        let pane = self.panes.get(&r.key)?;
        pane.tabs
            .iter()
            .find(|t| matches!(t.content, TabContent::Diff))
            .map(|t| t.id)
    }

    /// What the top-bar diff button does, which depends on where the diff lives.
    ///
    /// In `Dock` mode it is a visibility toggle. In `Tab` mode there is no dock
    /// to toggle, so it focuses the worktree's Diff tab, opening one first if
    /// there is none — the same "show me the diff" intent, landing wherever the
    /// setting put it.
    pub(crate) fn toggle_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.prefs.diff_location {
            DiffLocation::Dock => {
                self.prefs.diff_open = !self.prefs.diff_open;
                self.save_prefs(cx);
            }
            DiffLocation::Tab => match self.diff_tab_id() {
                Some(id) => {
                    let Some(r) = self.resolve() else { return };
                    if let Some(pane) = self.panes.get_mut(&r.key) {
                        pane.active = id;
                    }
                    self.focus_active_pane(window, cx);
                    self.save_panes();
                }
                None => self.open_tab(TabKind::Diff, window, cx),
            },
        }
        cx.notify();
    }

    /// Move the diff surface between the dock and the tab strip.
    ///
    /// Switching to `Dock` closes every open Diff tab across all live panes: a
    /// tab drawing a surface that has moved elsewhere is a tab that renders
    /// nothing, and leaving one behind would look like the setting half-worked.
    pub(crate) fn set_diff_location(
        &mut self,
        location: DiffLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prefs.diff_location == location {
            return;
        }
        self.prefs.diff_location = location;
        if location == DiffLocation::Dock {
            let stale: Vec<(String, u64)> = self
                .panes
                .iter()
                .flat_map(|(key, pane)| {
                    pane.tabs
                        .iter()
                        .filter(|t| matches!(t.content, TabContent::Diff))
                        .map(move |t| (key.clone(), t.id))
                })
                .collect();
            for (key, id) in stale {
                if let Some(pane) = self.panes.get_mut(&key) {
                    pane.tabs.retain(|t| t.id != id);
                    if pane.active == id {
                        pane.active = pane.tabs.last().map(|t| t.id).unwrap_or(0);
                    }
                }
            }
            // Moving the diff back to the dock should also show it there.
            self.prefs.diff_open = true;
            self.save_panes();
        }
        self.save_prefs(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }
```

- [ ] **Step 2: Gate the dock in the root render**

`workspace.rs` root render — replace the diff-panel `.when(...)`:

```rust
                    .when(
                        self.prefs.diff_open
                            && self.prefs.diff_location == DiffLocation::Dock,
                        |d| {
                            d.child(self.splitter(&hx, Drag::DiffPanel, cx))
                                .child(self.render_diff_panel(&hx, cx))
                        },
                    ),
```

- [ ] **Step 3: Render the Diff tab's content**

`workspace/ui/center.rs`, in `render_center`'s `match &tab.content`:

```rust
                TabContent::Diff => self.render_diff_surface(hx, None, cx),
```

(`render_diff_surface`'s second parameter is `Option<f32>`: `None` means "a tab, so full width"; `Some(width)` is the dock. Until Task 5 lands, use `self.render_diff_panel(hx, cx)` here so the build stays green.)

- [ ] **Step 4: Offer Diff in the ＋ menu, in Tab mode only**

`workspace/ui/center.rs`, in `render_tab_menu`, after the Editor item:

```rust
        // Only in Tab mode: in Dock mode the dock IS the surface, and a tab
        // showing the same review is the duplicate the setting exists to avoid.
        if self.prefs.diff_location == DiffLocation::Tab {
            menu = menu.child(self.tab_menu_item(hx, "⑂  Diff", "", TabKind::Diff, cx));
        }
```

Note: `menu` is built with `let mut menu = div()...child(...).child(...)` — insert this block after that statement and before the agent loop.

- [ ] **Step 5: Route the top-bar button through `toggle_diff`**

`workspace/ui/chrome.rs`, replace the `diff_toggle` `on_click` and its `text_color`:

```rust
            .text_color(
                if self.prefs.diff_location == DiffLocation::Dock && self.prefs.diff_open {
                    hx.tx1
                } else if self.prefs.diff_location == DiffLocation::Tab {
                    hx.tx1
                } else {
                    hx.tx2
                },
            )
            ...
            .on_click(cx.listener(|ws, _, window, cx| ws.toggle_diff(window, cx)));
```

The closure now needs `window`, so the listener signature changes from `|ws, _, _, cx|` to `|ws, _, window, cx|`.

- [ ] **Step 6: Add the setting to the settings screen**

`workspace/ui/settings.rs`, in the Extras section (after the pet toggle block, before the tmux hint):

```rust
        // ---- Diff panel ----------------------------------------------------
        {
            let location = self.prefs.diff_location;
            let mut row = div().flex().items_center().gap_1p5().pb_1();
            for (label, value) in [
                ("Right dock", DiffLocation::Dock),
                ("Tab", DiffLocation::Tab),
            ] {
                let selected = location == value;
                row = row.child(
                    div()
                        .id(gpui::SharedString::from(format!("diff-loc-{label}")))
                        .child(chip(label.to_string(), selected, hx))
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.set_diff_location(value, window, cx);
                        })),
                );
            }
            body = body.child(section_title("Diff panel")).child(row).child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .pb_1()
                    .child("A tab gives the diff the whole window; the dock keeps it beside your work."),
            );
        }
```

- [ ] **Step 7: Verify it builds and the suite still passes**

Run: `cd crates/harness-app && cargo test`
Expected: PASS, no warnings about unreachable or non-exhaustive matches.

- [ ] **Step 8: Manually verify**

Run: `make run`. Open Settings (⌘,) → Diff panel → **Tab**. The right dock disappears; the top-bar `diff ▤` button opens a `⑂ diff` tab; `＋` lists `⑂ Diff`. Switch back to **Right dock**: the diff tab disappears and the dock returns.

- [ ] **Step 9: Commit**

```bash
git add crates/harness-app/src
git commit -m "feat(diff): a setting to put the diff in a tab instead of the dock"
```

---

### Task 5: Two-pane layout

**Files:**
- Modify: `crates/harness-app/src/workspace/ui/diff.rs` (`render_diff_panel` ~351)
- Modify: `crates/harness-app/src/workspace.rs` (`Drag` ~359, `HarnessWorkspace` fields ~452, constructor ~680)
- Modify: `crates/harness-app/src/workspace/actions.rs` (`splitter` ~441, `drag_move` ~492)
- Modify: `crates/harness-app/src/workspace/state.rs` (`recompute_diff` ~827, `spawn_diff_compute` ~847)
- Modify: `crates/harness-app/src/workspace/ui/center.rs` (`render_comment_popover` ~238)

**Interfaces:**
- Consumes: `use_two_pane`, `DIFF_TWO_PANE_MIN` (Task 2); `DiffLocation` (Task 1).
- Produces: `HarnessWorkspace::render_diff_surface(&self, hx, dock_width: Option<f32>, cx) -> AnyElement`; `HarnessWorkspace::diff_focus: Option<String>`; `Drag::DiffList`.

- [ ] **Step 1: Add the focus field and the drag target**

`workspace.rs`, in `struct HarnessWorkspace`, next to `expanded`:

```rust
    /// The file whose diff the right pane shows, in the two-pane layout.
    ///
    /// Separate from `expanded` on purpose. The accordion needs a SET — reviewing
    /// means comparing two files, and collapsing one to open another loses your
    /// place. Two-pane needs exactly ONE. Sharing a field would have cost the
    /// accordion its multi-expand; keeping both means neither layout has to be
    /// reconciled when the other is in use.
    diff_focus: Option<String>,
```

Initialize it to `None` in the constructor alongside `expanded`.

`workspace.rs`, in `enum Drag`:

```rust
    /// File-list column width inside the two-pane diff surface.
    DiffList,
```

`workspace/actions.rs`, in `splitter`: add `Drag::DiffList => self.prefs.diff_list_width,` to `start_value` and `Drag::DiffList => "split-diff-list",` to the `.id(...)` match.

`workspace/actions.rs`, in `drag_move`: add `Drag::DiffList => (ev.position.x, 1.0),` to the position match (the list is left-anchored, so dragging right widens it) and `Drag::DiffList => self.prefs.diff_list_width = next,` to the assignment match.

- [ ] **Step 2: Keep `diff_focus` honest across refreshes**

`workspace/state.rs`, in `recompute_diff`, next to `self.expanded.clear();`:

```rust
        self.diff_focus = None;
```

In `spawn_diff_compute`, inside the `view.update` closure after `ws.diff = computed;`:

```rust
                // A file can stop changing while it is being read. Drop a focus
                // that no longer names a changed file rather than leaving the
                // right pane pointed at a diff that is gone.
                if let Some(f) = ws.diff_focus.as_ref()
                    && !ws.diff.files.iter().any(|file| &file.path == f)
                {
                    ws.diff_focus = None;
                }
```

- [ ] **Step 3: Split the panel into a surface plus two layouts**

In `workspace/ui/diff.rs`, rename the body of `render_diff_panel` into `render_diff_surface` and reduce `render_diff_panel` to the dock chrome:

```rust
    /// The dock: fixed width, its own border, wrapping the shared surface.
    pub(crate) fn render_diff_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.diff_width))
            .h_full()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(self.render_diff_surface(hx, Some(self.prefs.diff_width), cx))
            .into_any_element()
    }

    /// The diff review surface, drawn identically in the dock and in a tab.
    ///
    /// `dock_width` is `Some` for the dock and `None` for a tab, which is the
    /// only thing the two differ by: a tab always has room for two columns.
    pub(crate) fn render_diff_surface(
        &self,
        hx: &HarnessTheme,
        dock_width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(r) = self.resolve() else {
            return self.render_no_project(hx).into_any_element();
        };
        let two_pane = use_two_pane(
            self.prefs.diff_location,
            dock_width.unwrap_or(f32::INFINITY),
        );
        let (adds, dels) = self.diff.totals();
        let review_count = self.review.count(&r.key);

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(self.render_diff_header(hx, &r, adds, dels, cx));
        if review_count > 0 {
            panel = panel.child(self.render_review_bar(hx, review_count, cx));
        }
        if two_pane {
            panel = panel.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_file_list(hx, true, cx))
                    .child(self.splitter(hx, Drag::DiffList, cx))
                    .child(self.render_focused_diff(hx, cx)),
            );
        } else {
            panel = panel.child(self.render_file_list(hx, false, cx));
        }
        panel.into_any_element()
    }
```

Extract the existing "No project selected" block into `render_no_project(&self, hx) -> impl IntoElement` (same children, but without the `.w(px(...))`/`.border_l_1()` — the dock wrapper owns those now), and the existing header + "Changed files N/M reviewed" + totals rows into `render_diff_header(&self, hx, r: &Resolved, adds: usize, dels: usize, cx) -> impl IntoElement`. Both are pure moves of code that is already there.

- [ ] **Step 4: Make the file list serve both layouts**

Convert the existing file-row loop into `render_file_list(&self, hx: &HarnessTheme, two_pane: bool, cx: &mut Context<Self>) -> AnyElement`. Three changes to the body that is already there:

1. The container gets a width in two-pane mode:

```rust
        let mut files = div()
            .id("diff-files")
            .flex()
            .flex_col()
            .min_h_0()
            .items_start()
            .overflow_scroll()
            .px_2()
            .pb_1();
        files = if two_pane {
            files.flex_none().w(px(self.prefs.diff_list_width))
        } else {
            files.flex_1()
        };
```

2. A row's meaning depends on the layout — select in two-pane, expand in the accordion:

```rust
                    .when(two_pane && self.diff_focus.as_deref() == Some(path.as_str()), |d| {
                        d.bg(hx.surface)
                    })
                    .when(!two_pane && expanded, |d| d.bg(hx.surface))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if two_pane {
                            ws.diff_focus = Some(toggle_path.clone());
                        } else if !ws.expanded.remove(&toggle_path) {
                            ws.expanded.insert(toggle_path.clone());
                        }
                        cx.notify();
                    }))
```

3. The disclosure marker and the inline body are accordion-only — wrap both in `.when(!two_pane, ...)`, and skip the `if expanded { ... }` block entirely when `two_pane`.

The reviewed checkbox, change badge, path, markdown toggle and `+`/`−` counts are unchanged. In two-pane the markdown toggle's `expanded &&` guard becomes `self.diff_focus.as_deref() == Some(path.as_str())`.

- [ ] **Step 5: Add the right pane**

New method in `workspace/ui/diff.rs`:

```rust
    /// The right-hand column: the focused file's diff, or its Markdown preview.
    ///
    /// Falls back to the first changed file so the pane is never blank while
    /// there is something to review — an empty column beside a populated list
    /// reads as a failure to load rather than as "nothing selected".
    fn render_focused_diff(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let focus = self
            .diff_focus
            .clone()
            .or_else(|| self.diff.files.first().map(|f| f.path.clone()));
        let Some(file) = focus else {
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("no changes vs merge-base")
                .into_any_element();
        };
        let body = match self.md_preview.as_ref().filter(|(f, _)| f == &file) {
            Some((_, md)) => div().size_full().child(md.clone()).into_any_element(),
            None => self.render_diff_body(hx, &file, cx),
        };
        div()
            .id("diff-focused")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_scroll()
            .p_1()
            .child(body)
            .into_any_element()
    }
```

- [ ] **Step 6: Make the comment popover side-aware**

`workspace/ui/center.rs`, in `render_comment_popover`, replace the hard-coded left calculation:

```rust
        // Open away from the nearer edge. The dock is on the right, so a
        // gutter click there must open leftward; in a tab the diff can sit
        // anywhere, and opening left of a left-hand click would run off-window.
        let window_width = f32::from(window_bounds_width);
        let anchor_x = f32::from(draft.at.x);
        let left = if anchor_x > window_width / 2.0 {
            (anchor_x - W - 14.0).max(8.0)
        } else {
            (anchor_x + 14.0).min((window_width - W - 8.0).max(8.0))
        };
```

`render_comment_popover` has no `Window` today, so thread one in: change its signature to `(&self, hx: &HarnessTheme, draft: &CommentDraft, window: &Window, cx: &mut Context<Self>)`, take `let window_bounds_width = window.viewport_size().width;` at the top, and pass `window` at the call site in `workspace.rs`'s root render.

- [ ] **Step 7: Verify it builds and the suite passes**

Run: `cd crates/harness-app && cargo test`
Expected: PASS.

- [ ] **Step 8: Manually verify**

Run: `make run`.
- Dock at its default 340px: unchanged accordion.
- Drag the dock splitter past ~560px: it becomes two columns; the inner splitter resizes the file list; both widths survive a restart.
- Settings → Tab: two columns filling the window; clicking a file swaps the right pane; the comment popover opens on the sensible side at both edges.

- [ ] **Step 9: Commit**

```bash
git add crates/harness-app/src
git commit -m "feat(diff): two-pane files/changes layout with a resizable list"
```

---

### Task 6: Edit intent — offer an editor when the user tries to type

**Files:**
- Create: `crates/harness-app/src/workspace/ui/diff_edit.rs`
- Modify: `crates/harness-app/src/workspace/ui.rs` (module list)
- Modify: `crates/harness-app/src/workspace_host.rs` (new `open_file_at` beside `probe_open` ~552)
- Modify: `crates/harness-app/src/workspace.rs` (`EditPrompt` struct, field, root render)
- Modify: `crates/harness-app/src/workspace/state.rs` (`request_edit`, `open_in_editor`)
- Modify: `crates/harness-app/src/workspace/ui/diff.rs` (row `on_mouse_down` ~242, key handler)

**Interfaces:**
- Consumes: `target_line_for_row` (Task 2); `readable_diff_lines`; `TabContent::Editor`.
- Produces: `workspace_host::open_file_at(hosted: &Entity<::workspace::Workspace>, rel: &str, line: Option<u32>, window: &mut Window, cx: &mut App)`; `HarnessWorkspace::request_edit(&mut self, file: String, line: Option<u32>, window, cx)`; `struct EditPrompt { file: String, line: Option<u32> }`; `HarnessWorkspace::edit_prompt: Option<EditPrompt>`.

- [ ] **Step 1: Add the file-opening helper**

In `workspace_host.rs`, next to `probe_open`:

```rust
/// Open `rel` (worktree-relative) in a hosted workspace, optionally putting the
/// caret on `line` (1-based, as the diff gutter shows it).
///
/// The same recipe `probe_open` uses, promoted to a real entry point: a
/// `RelPath` into a `ProjectPath` into `Workspace::open_path`, then a selection
/// change so the file opens *where the user was looking* rather than at line 1.
pub fn open_file_at(
    hosted: &Entity<Workspace>,
    rel: &str,
    line: Option<u32>,
    window: &mut Window,
    cx: &mut App,
) {
    let Ok(rel) = util::rel_path::RelPath::new(Path::new(rel), util::paths::PathStyle::local())
    else {
        log::warn!("open_file_at: {rel:?} is not a worktree-relative path");
        return;
    };
    let rel: Arc<util::rel_path::RelPath> = rel.as_ref().into();

    let task = hosted.update(cx, |ws, cx| {
        let worktree = ws.project().read(cx).visible_worktrees(cx).next()?;
        let path = project::ProjectPath { worktree_id: worktree.read(cx).id(), path: rel };
        Some(ws.open_path(path, None, true, window, cx))
    });
    let Some(task) = task else { return };

    let hosted = hosted.clone();
    window
        .spawn(cx, async move |cx| {
            let item = task.await?;
            hosted.update_in(cx, |_, window, cx| {
                let Some(editor) = item.act_as::<editor::Editor>(cx) else {
                    return;
                };
                if let Some(line) = line {
                    editor.update(cx, |editor, cx| {
                        // 1-based in the gutter, 0-based in the buffer.
                        let row = line.saturating_sub(1);
                        editor.change_selections(
                            editor::SelectionEffects::default(),
                            window,
                            cx,
                            |s| {
                                s.select_ranges([
                                    language::Point::new(row, 0)..language::Point::new(row, 0)
                                ]);
                            },
                        );
                    });
                }
                window.focus(&editor.focus_handle(cx), cx);
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
}
```

Add `use gpui::Focusable as _;` if the file does not already import it, and confirm `editor::SelectionEffects` is the correct public path — if it is not exported there, use the path `cargo check`'s error message names.

- [ ] **Step 2: Add the request/open state**

`workspace.rs`, near `ClosePrompt`:

```rust
/// A pending "you tried to edit a diff line — where should it open?" question.
pub(crate) struct EditPrompt {
    file: String,
    /// The working-file line to land on, or `None` when the row was a deletion
    /// with nothing after it.
    line: Option<u32>,
}
```

Add `edit_prompt: Option<EditPrompt>,` to `HarnessWorkspace` and `edit_prompt: None,` to the constructor.

`workspace/state.rs`:

```rust
    /// The user tried to edit a diff line. A diff is a picture of a change, not
    /// the file, so the only useful response is to take them somewhere they can
    /// actually type.
    ///
    /// With no editor open there is no choice to make — open one. With editors
    /// already open, ask: dropping the file into one of several tabs the user
    /// did not name is a guess, and a wrong guess scrolls a file they were
    /// reading out from under them.
    pub(crate) fn request_edit(
        &mut self,
        file: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let deleted = self
            .diff
            .files
            .iter()
            .any(|f| f.path == file && f.change == Change::Deleted);
        if deleted {
            self.note = format!("{file} is deleted in this branch");
            cx.notify();
            return;
        }
        if self.editor_tab_ids().is_empty() {
            self.open_in_editor(file, line, true, window, cx);
        } else {
            self.edit_prompt = Some(EditPrompt { file, line });
            cx.notify();
        }
    }

    /// Ids of this worktree's editor tabs, most recently opened last.
    pub(crate) fn editor_tab_ids(&self) -> Vec<u64> {
        let Some(r) = self.resolve() else { return Vec::new() };
        let Some(pane) = self.panes.get(&r.key) else { return Vec::new() };
        pane.tabs
            .iter()
            .filter(|t| matches!(t.content, TabContent::Editor(_)))
            .map(|t| t.id)
            .collect()
    }

    /// Open `file` at `line` in an editor tab: a fresh one when `new_tab`, else
    /// the most recent existing one. Either way the center switches to it —
    /// opening a file in a tab the user cannot see is indistinguishable from
    /// having done nothing.
    pub(crate) fn open_in_editor(
        &mut self,
        file: String,
        line: Option<u32>,
        new_tab: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.edit_prompt = None;
        if new_tab {
            self.open_tab(TabKind::Editor, window, cx);
        } else if let Some(id) = self.editor_tab_ids().last().copied() {
            let Some(r) = self.resolve() else { return };
            if let Some(pane) = self.panes.get_mut(&r.key) {
                pane.active = id;
            }
        }
        let Some(r) = self.resolve() else { return };
        let hosted = self.panes.get(&r.key).and_then(|p| {
            p.tabs.iter().find(|t| t.id == p.active).and_then(|t| match &t.content {
                TabContent::Editor(ws) => Some(ws.clone()),
                _ => None,
            })
        });
        let Some(hosted) = hosted else {
            self.note = "could not open an editor".to_string();
            cx.notify();
            return;
        };
        crate::workspace_host::open_file_at(&hosted, &file, line, window, cx);
        self.save_panes();
        cx.notify();
    }
```

`Change` is already in scope via `use super::super::*`; if not, import `worktrees::Change`.

- [ ] **Step 3: Write the prompt UI**

Create `crates/harness-app/src/workspace/ui/diff_edit.rs`:

```rust
//! The "you tried to edit a diff line" question (§3.4).
//!
//! Split out of `diff.rs` rather than added to it: that file already renders
//! the panel, the file list, the rows and the review bar, and a modal is a
//! separate concern that happens to be triggered from there.

use super::super::*;

impl HarnessWorkspace {
    pub(crate) fn render_edit_prompt(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(prompt) = self.edit_prompt.as_ref() else {
            return div().into_any_element();
        };
        let where_ = match prompt.line {
            Some(l) => format!("{}:{l}", prompt.file),
            None => prompt.file.clone(),
        };
        let (file, line) = (prompt.file.clone(), prompt.line);
        let (file2, line2) = (prompt.file.clone(), prompt.line);

        let button = |id: &'static str, label: String, primary: bool| {
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .h(px(34.0))
                .rounded_md()
                .border_1()
                .border_color(if primary { hx.accent } else { hx.border2 })
                .bg(if primary { tint(hx.accent, 0.16) } else { hx.surface })
                .text_sm()
                .text_color(if primary { hx.accent } else { hx.tx0 })
                .cursor_pointer()
                .hover(|d| d.border_color(hx.accent).text_color(hx.accent))
                .child(label)
        };

        let card = div()
            .w(px(420.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child("Open this file to edit it?"),
            )
            .child(
                div()
                    .text_sm()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx1)
                    .child(where_),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("The diff is a picture of the change — edits happen in the editor."),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(button("edit-existing", "Open in editor".into(), true).on_click(
                        cx.listener(move |ws, _, window, cx| {
                            ws.open_in_editor(file.clone(), line, false, window, cx)
                        }),
                    ))
                    .child(button("edit-new-tab", "New editor tab".into(), false).on_click(
                        cx.listener(move |ws, _, window, cx| {
                            ws.open_in_editor(file2.clone(), line2, true, window, cx)
                        }),
                    )),
            );

        div()
            .id("edit-backdrop")
            .absolute()
            .top(px(0.0))
            .left(px(0.0))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x00000099))
            // Clicking outside cancels — reading a diff is the common case, and
            // an accidental double-click must not cost you your place.
            .on_mouse_down(MouseButton::Left, cx.listener(|ws, _, _, cx| {
                ws.edit_prompt = None;
                cx.notify();
            }))
            .child(card)
            .into_any_element()
    }
}
```

Add `mod diff_edit;` to `crates/harness-app/src/workspace/ui.rs`.

In `workspace.rs`'s root render, beside the close-prompt block:

```rust
        if self.edit_prompt.is_some() {
            root = root.child(self.render_edit_prompt(&hx, cx));
        }
```

- [ ] **Step 4: Wire the double-click trigger**

`workspace/ui/diff.rs`, in `render_diff_body`'s row `on_mouse_down`. The rows are built from `shown` (a `Vec<DiffRow>`); clone what the closure needs before the loop and use it here:

```rust
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, ev: &MouseDownEvent, window, cx| {
                                // A second click on a diff line means "let me
                                // change this" — the diff itself cannot be
                                // typed into, so offer the file that can.
                                if ev.click_count >= 2 {
                                    ws.diff_selecting = false;
                                    let line = target_line_for_row(&rows_for_edit, ix);
                                    ws.request_edit(dbl_file.clone(), line, window, cx);
                                    return;
                                }
                                ws.diff_selecting = true;
                                ws.select_diff_row(&row_file, ix, ev.modifiers.shift, cx);
                            }),
                        )
```

Before the loop:

```rust
            let rows_for_edit = std::sync::Arc::new(shown.clone());
```

and inside the loop, per row: `let rows_for_edit = rows_for_edit.clone(); let dbl_file = file_for_comments.clone();`. `DiffRow` needs `#[derive(Clone)]` — add it if absent.

- [ ] **Step 5: Wire the typing trigger**

On the `preview` container in `render_diff_body`, after `.font_family(hx.font_mono)`:

```rust
            // Typing with a line selected is the other way people say "I want
            // to change this". Modified chords are shortcuts, not text, so they
            // pass through untouched.
            .on_key_down(cx.listener(|ws, ev: &gpui::KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.modifiers.command || k.modifiers.control || k.modifiers.alt {
                    return;
                }
                if k.key_char.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    return;
                }
                let Some((file, from, _)) = ws.diff_selection.clone() else { return };
                let rows = readable_diff_lines(&ws.unified_for_file(&file));
                let line = target_line_for_row(&rows, from);
                ws.request_edit(file, line, window, cx);
            }))
```

- [ ] **Step 6: Verify it builds and the suite passes**

Run: `cd crates/harness-app && cargo test`
Expected: PASS.

- [ ] **Step 7: Manually verify**

Run: `make run`. In a worktree with changes:
- With no editor tab open, double-click a `+` line → an editor tab opens with that file, caret on that line.
- With an editor tab open, double-click → the question appears; **Open in editor** reuses it, **New editor tab** makes another, clicking the backdrop cancels.
- Double-click a `−` line → lands on the following surviving line, not an unrelated one.
- Select a line and type a letter → same question. ⌘C on a selection still copies.

- [ ] **Step 8: Commit**

```bash
git add crates/harness-app/src
git commit -m "feat(diff): offer an editor when the user tries to edit a diff line"
```

---

### Task 7: Right-align the Markdown preview eye

**Files:**
- Modify: `crates/harness-app/Cargo.toml` (dependencies)
- Modify: `crates/harness-app/src/workspace_host.rs` (`init_toolbar` ~191–224)

**Interfaces:**
- Produces: nothing other crates consume; a `breadcrumbs::Breadcrumbs` item registered as `PrimaryLeft` on every pane toolbar.

- [ ] **Step 1: Add the dependency**

`crates/harness-app/Cargo.toml`, beside the other zed toolbar crates:

```toml
# Zed's breadcrumb trail (file path › enclosing symbol) as a PrimaryLeft toolbar
# item. Not cosmetic: `Toolbar::render` right-aligns the PrimaryRight group with
# `justify_between`, which only pushes right when a LEFT item exists. With none,
# the group collapses to the left edge and takes the Markdown preview eye with
# it — zed always has this item, and we did not.
breadcrumbs = { git = "https://github.com/zed-industries/zed.git", rev = "952d712dac48a4af2c54fb22c82d82a9d69b72d4" }
```

- [ ] **Step 2: Register the item**

`workspace_host.rs`, inside `init_toolbar`, alongside the other constructors:

```rust
            let breadcrumbs = cx.new(|_| breadcrumbs::Breadcrumbs::new());
```

and inside the `toolbar.update`, as the FIRST `add_item`:

```rust
                    // First, and PrimaryLeft: this is what pushes the eye (and
                    // any future right-hand item) to the right edge. See the
                    // Cargo.toml note.
                    toolbar.add_item(breadcrumbs, window, cx);
```

- [ ] **Step 3: Verify it builds**

Run: `cd crates/harness-app && cargo check`
Expected: clean. The first build after adding the dependency compiles new crates and is slow.

- [ ] **Step 4: Manually verify**

Run: `make run`. Open an editor tab, open a `.md` file: the breadcrumb trail sits at the left of the toolbar row and the eye at the right. Open a `.rs` file: breadcrumbs remain, the eye is gone (it is `Hidden` on non-Markdown), and no empty strip appears above non-editor items.

- [ ] **Step 5: Commit**

```bash
git add crates/harness-app/Cargo.toml crates/harness-app/Cargo.lock crates/harness-app/src/workspace_host.rs
git commit -m "fix(editor): right-align the Markdown preview eye via zed's breadcrumbs"
```

---

### Task 8: Drag to reorder tabs within a worktree

**Files:**
- Modify: `crates/harness-app/src/workspace.rs` (a drag-payload type; tests)
- Modify: `crates/harness-app/src/workspace/ui/center.rs` (`render_tab` ~143)
- Modify: `crates/harness-app/src/workspace/state.rs` (`move_tab`)

**Interfaces:**
- Consumes: `PaneSet`, `Tab`, `save_panes` (existing).
- Produces: `struct DraggedTab { key: String, id: u64 }`; `HarnessWorkspace::move_tab(&mut self, key: &str, dragged: u64, onto: u64)`.

- [ ] **Step 1: Write the failing test**

`move_tab` reorders a `Vec<Tab>`, but `Tab` holds live entities and cannot be built in a unit test. Test the ordering rule as a free function over ids, and have `move_tab` call it:

```rust
#[test]
fn dragging_a_tab_onto_another_puts_it_at_that_position() {
    // Forward: 1 dropped onto 3 lands after the tabs it passed.
    assert_eq!(reordered(&[1, 2, 3, 4], 1, 3), vec![2, 3, 1, 4]);
    // Backward: 4 dropped onto 2 lands where 2 was.
    assert_eq!(reordered(&[1, 2, 3, 4], 4, 2), vec![1, 4, 2, 3]);
}

#[test]
fn a_tab_dropped_on_itself_or_on_a_stranger_changes_nothing() {
    // Both cases arise for real: a click that gpui reports as a tiny drag, and
    // a drop onto a tab strip belonging to a different worktree.
    assert_eq!(reordered(&[1, 2, 3], 2, 2), vec![1, 2, 3]);
    assert_eq!(reordered(&[1, 2, 3], 9, 2), vec![1, 2, 3]);
    assert_eq!(reordered(&[1, 2, 3], 2, 9), vec![1, 2, 3]);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd crates/harness-app && cargo test dragging_a_tab`
Expected: FAIL — `cannot find function reordered in this scope`.

- [ ] **Step 3: Implement the rule and the payload**

`workspace.rs`:

```rust
/// The tab being dragged, and the pane set it came from.
///
/// Carries the worktree key so a drop can refuse a tab from somewhere else:
/// tab sets are per worktree, and moving one across would mean moving a live
/// pty into a directory it was not spawned in.
pub(crate) struct DraggedTab {
    pub key: String,
    pub id: u64,
}

/// `ids` with `dragged` moved to `onto`'s position. Unknown or identical ids
/// leave the order untouched.
fn reordered(ids: &[u64], dragged: u64, onto: u64) -> Vec<u64> {
    let mut out = ids.to_vec();
    if dragged == onto {
        return out;
    }
    let (Some(from), Some(to)) = (
        out.iter().position(|i| *i == dragged),
        out.iter().position(|i| *i == onto),
    ) else {
        return out;
    };
    let id = out.remove(from);
    out.insert(to, id);
    out
}
```

`workspace/state.rs`:

```rust
    /// Move a tab within its own pane set, then persist the new order.
    pub(crate) fn move_tab(&mut self, key: &str, dragged: u64, onto: u64) {
        let Some(pane) = self.panes.get_mut(key) else { return };
        let ids: Vec<u64> = pane.tabs.iter().map(|t| t.id).collect();
        let order = reordered(&ids, dragged, onto);
        if order == ids {
            return;
        }
        // Sort in place by the new order's index, so the live entities move
        // with their tabs rather than being rebuilt.
        pane.tabs.sort_by_key(|t| order.iter().position(|i| *i == t.id).unwrap_or(usize::MAX));
        self.save_panes();
    }
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cd crates/harness-app && cargo test reorder && cd crates/harness-app && cargo test dragging_a_tab`
Expected: PASS.

- [ ] **Step 5: Make tabs draggable**

`workspace/ui/center.rs`, in `render_tab` — the method needs the worktree key, so read it at the top:

```rust
        let key = self.resolve().map(|r| r.key).unwrap_or_default();
        let drag_key = key.clone();
        let drop_key = key.clone();
        let onto = tab.id;
        let title = tab.title.clone();
        let hx_ghost = hx.clone();
```

and on the tab's root div, after `.on_click(...)`:

```rust
            // The ghost that follows the cursor. gpui needs a real view here,
            // so it is a one-off entity rendering the tab's own label — a
            // generic rectangle would not say which tab is moving.
            .on_drag(DraggedTab { key: drag_key, id }, move |_, _, _, cx| {
                let label = title.clone();
                let hx = hx_ghost.clone();
                cx.new(|_| DragGhost { label, hx })
            })
            .drag_over::<DraggedTab>(move |d, _, _, _| d.bg(hx.surface_hover))
            .on_drop(cx.listener(move |ws, dragged: &DraggedTab, _, cx| {
                // Same worktree only: a tab holds a pty spawned in this
                // worktree's directory, so moving it elsewhere would silently
                // point a running agent at the wrong tree.
                if dragged.key != drop_key {
                    return;
                }
                ws.move_tab(&drop_key, dragged.id, onto);
                cx.notify();
            }))
```

Add the ghost view to `workspace/ui/center.rs`:

```rust
/// The label that follows the cursor while a tab is being dragged.
pub(crate) struct DragGhost {
    label: String,
    hx: HarnessTheme,
}

impl Render for DragGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_md()
            .bg(self.hx.surface2)
            .border_1()
            .border_color(self.hx.accent)
            .text_xs()
            .font_family(self.hx.font_mono)
            .text_color(self.hx.tx0)
            .child(self.label.clone())
    }
}
```

`HarnessTheme` must be `Clone` for this — it already is (the root render does `theme(cx).clone()`).

- [ ] **Step 6: Verify it builds and the suite passes**

Run: `cd crates/harness-app && cargo test`
Expected: PASS.

- [ ] **Step 7: Manually verify**

Run: `make run`. Drag a tab left and right within one worktree: the strip reorders, a labelled ghost follows the cursor, the dragged tab stays active and its terminal keeps running. Switch worktrees and back — the order persists. Restart the app — the order persists.

- [ ] **Step 8: Commit**

```bash
git add crates/harness-app/src
git commit -m "feat(tabs): drag to reorder tabs within a worktree"
```

---

## Self-Review

**Spec coverage.** §1 location → Tasks 1, 3, 4. §2 two-pane → Tasks 1, 2, 5. §3 edit intent → Tasks 2, 6. §4 the eye → Task 7. Testing section → the pure functions in Tasks 1, 2, 3, 8. Tab drag-reordering (requested after the spec was written) → Task 8. No spec section is unclaimed.

**Type consistency.** `DiffLocation` (Task 1) is consumed by `use_two_pane` (Task 2), the root render and `set_diff_location` (Task 4), and `render_diff_surface` (Task 5) — same name and variants throughout. `target_line_for_row(&[DiffRow], usize) -> Option<u32>` (Task 2) is called in Task 6 with exactly that signature, and `Option<u32>` flows unchanged through `request_edit` → `open_in_editor` → `open_file_at`. `TabKind::Diff`/`TabContent::Diff`/`TabRecordKind::Diff` (Task 3) are used under those names in Tasks 4 and 5.

**Known risks, to resolve against the compiler rather than by guessing.** `editor::SelectionEffects` and `gpui::KeyDownEvent::keystroke.key_char` are the two paths most likely to have moved at this zed rev; both fail at compile time, and the error names the right path. `render_comment_popover` gaining a `Window` parameter means its single call site must be updated in the same edit.
