# Diff Header Narrow Overlap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop the diff panel's header from overlapping itself when the panel is dragged to its 220px minimum width.

**Architecture:** Two independent layout defects in `render_diff_header`. Row 1's branch name wraps inside a fixed-height row and bleeds over its neighbours — fixed by `truncate()`. Row 2's two `justify_between` groups need ~375px and get 196px — fixed by letting the row `flex_wrap()` so the groups stack instead of colliding.

**Tech Stack:** Rust, gpui (zed rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4`).

## Global Constraints

- **Design spec:** `docs/superpowers/specs/2026-08-09-diff-header-narrow-overlap-design.md`. It is the authority; this plan implements it.
- **`crates/harness-app` is outside the root cargo workspace.** A root `cargo build` or `make test` **skips it and still reports success**. Every build/test command must be run from inside `crates/harness-app`.
- **This change is CSS-equivalent only.** Do not alter `SPLIT_MIN_W`, the `diff_width` clamp at `workspace.rs:2157`, any rendered string, or any behaviour. If a step seems to require changing what the header *says* or *does*, stop — the plan is wrong, not the constraint.
- **No new constants and no width thresholds.** The whole point of the wrap approach is that it needs neither.
- **No automated tests.** The spec records why: pure layout, no extractable logic, no layout harness in the repo. Do not invent a test that asserts a style property — it would test gpui, not this fix.

---

### Task 1: Stop the branch name wrapping

Row 1 in isolation. This is the visible bleed in the bug report and is a defect at any panel width.

**Files:**
- Modify: `crates/harness-app/src/workspace/ui/diff.rs:1108`

**Interfaces:**
- Consumes: nothing.
- Produces: nothing. Self-contained styling change.

- [ ] **Step 1: Read the surrounding function first**

Read `crates/harness-app/src/workspace/ui/diff.rs:1084-1162` (`fn render_diff_header`) in full before editing, so the two rows are clear. Task 2 changes the second row; do not touch it here.

- [ ] **Step 2: Apply the change**

Replace line 1108 exactly:

```rust
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(r.branch.clone())),
```

with:

```rust
                    // `truncate()` (= overflow_hidden + whitespace_nowrap +
                    // text_ellipsis), not bare `overflow_hidden()`: clipping
                    // stops a box painting outside itself but does nothing about
                    // *wrapping*, and a long branch wrapped to three lines inside
                    // this `h(px(34.0))` row overflowed it above and below —
                    // painting over the row beneath.
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).truncate().child(r.branch.clone())),
```

- [ ] **Step 3: Verify it compiles**

```bash
cd crates/harness-app && cargo check
```

Expected: clean. If `truncate` is not found, confirm `gpui::prelude::*` (which re-exports `Styled`) is in scope in this file — it is, via `use super::super::*`.

- [ ] **Step 4: Commit**

```bash
git add crates/harness-app/src/workspace/ui/diff.rs
git commit -m "fix(diff): keep a long branch name on one line in the header"
```

---

### Task 2: Let the control row wrap

**Files:**
- Modify: `crates/harness-app/src/workspace/ui/diff.rs:1110-1160`

**Interfaces:**
- Consumes: nothing from Task 1 (different row; the tasks are independent).
- Produces: nothing.

- [ ] **Step 1: Make the row wrappable**

In the second `.child(` block starting at line 1110, the row currently opens:

```rust
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
```

Replace with:

```rust
                div()
                    .flex()
                    .flex_none()
                    // Below ~400px the two groups cannot share a line: the left
                    // wants ~207px and the right ~160px, against 196px of usable
                    // width. Wrapping stacks them instead of letting them paint
                    // over each other. `space-between` with one item on a line
                    // degenerates to `flex-start`, so both lines left-align with
                    // no override needed.
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_x_2()
                    .gap_y_1()
                    .px_3()
                    .py_2()
```

- [ ] **Step 2: Let the left group actually shrink**

Inside the left group (the `.child(` opening at line 1117), find the `Changed files …` label — the `div()` carrying `font_weight(gpui::FontWeight::SEMIBOLD)` and `text_color(hx.tx0)` whose child is the `if self.diff_pending { … }` expression. Add `.truncate()` to that `div()`, immediately after `.text_color(hx.tx0)`:

```rust
                            .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).truncate().child(
```

The group already has `min_w_0()`. That has been inert: with no truncatable text child, the group's minimum content width never dropped, so `min_w_0()` had nothing to permit.

- [ ] **Step 3: Let the right group shrink, but aim where the shrinking lands**

In the right group (the `.child(` opening at line 1149), add `.min_w_0()` after `.flex()` — **not** `.flex_none()`:

```rust
                    .child(
                        div()
                            .flex()
                            // `min_w_0`, not `flex_none`: the group must take a
                            // whole line once wrapped, but it must still be able
                            // to shrink onto it. Pinned unshrinkable, the armed
                            // push control would hang off the panel's right edge
                            // — the same overflow this row exists to fix.
                            // Shrinkage is absorbed by the push label alone; the
                            // toggle (`flex_none` internally) and the counts hold
                            // their size, because a squeezed toggle is
                            // unclickable and a clipped count is a wrong number.
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
```

and give the two count children `.flex_none()` so the squeeze cannot reach them:

```rust
                            .child(div().flex_none().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().flex_none().text_color(hx.red).child(format!("−{dels}"))),
```

Do not add `flex_wrap()` to either inner group. Each stays one line and wraps as a unit.

- [ ] **Step 4: Bound the push control**

`render_push_control` (`diff.rs:890`) is the one control here whose width is unbounded and data-dependent: armed, it renders `push N → origin/<branch>?`, which for a real branch name is ~345px against ~196px of header. Add `.min_w_0().truncate()` to its root `div()`, after `.flex()`:

```rust
            div()
                .id("push-branch")
                .flex()
                // Armed, this reads `push 3 → origin/<branch>?` — ~345px for a
                // real branch name, against ~196px of header at the panel's
                // minimum width. It is the one control here that is unbounded,
                // so it is the one that gives ground.
                .min_w_0()
                .truncate()
                .items_center()
                .h(px(20.0))
                .px_1p5()
```

- [ ] **Step 5: Add the backstop**

On the row-2 container, alongside `.flex_wrap()`, add `.overflow_hidden()`:

```rust
                    .flex_wrap()
                    // Hard backstop. Wrapping and shrinking between them should
                    // keep everything inside, but the failure mode being fixed
                    // here is content painting over its neighbours, and clipping
                    // is a strictly better way to lose than that. The base menu
                    // is unaffected: it renders at the workspace root
                    // (`workspace.rs:1748`), not inside this row.
                    .overflow_hidden()
```

Confirm before adding this that the base-diff dropdown is **not** a descendant of this row — it is rendered at `workspace.rs:1748`, so clipping here cannot clip the menu. If that ever changes, this backstop has to move.

- [ ] **Step 6: Verify it compiles**

```bash
cd crates/harness-app && cargo check
```

Expected: clean.

- [ ] **Step 7: Run the crate's tests**

```bash
cd crates/harness-app && cargo test
```

Expected: pass, unchanged. This task adds no tests; it must not break existing ones.

- [ ] **Step 8: Re-read your own diff**

```bash
git diff crates/harness-app/src/workspace/ui/diff.rs
```

Confirm against the Global Constraints: no changed strings, no changed behaviour, no new constants, no touched thresholds. Style calls and comments only.

- [ ] **Step 9: Commit**

```bash
git add crates/harness-app/src/workspace/ui/diff.rs
git commit -m "fix(diff): wrap the header's control row instead of overlapping it"
```

---

### Task 3: Visual verification

**Files:** none modified.

**Interfaces:**
- Consumes: Tasks 1 and 2.
- Produces: a verification verdict from the user.

- [ ] **Step 1: Hand the checks to the user**

This is verified by eye, in a real build, at the drag extremes. The build is produced once for both this fix and the terminal-file-drop feature; do not run `make app` here if that build already covers this branch's commits.

Checks, on the diff panel:

1. Drag the panel to its **narrowest**. The branch name is one line ending in `…`, inside its row. Base button and file count on one line; view toggle and `+N −M` on the line below. Nothing overlaps.
2. Widen slowly through ~400px. The lines rejoin into one — a reflow, not a jump or flicker.
3. At a **wide** panel, the header is identical to before this change.
4. A task with a **short** branch name shows no stray `…`.
5. With the panel narrow, the base dropdown still opens on click, positioned at the cursor.

- [ ] **Step 2: STOP until the user confirms**

Do not open a pull request until the user has run the build and confirmed. If any check fails, fix it, rebuild, and hand it back.
