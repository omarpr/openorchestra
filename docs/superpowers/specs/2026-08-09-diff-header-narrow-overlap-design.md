# Diff header at minimum panel width

**Date:** 2026-08-09
**Status:** approved (design)

## Problem

Drag the diff panel to its narrowest and the header tears itself apart: the
branch name spills out of its row and paints over the row beneath it, and the
base button, file count, view toggle, and +/− stats all pile onto the same
pixels. Nothing is readable and nothing indicates which control is where.

`diff_width` clamps at a **220px** minimum (`workspace.rs:2157`). The header
needs roughly **375px** of content plus 24px of `px_3` padding to lay out. At
the minimum it is given about half the width it needs, and nothing in the markup
says how to give ground.

### Two independent failures

**Row 1 — the visible bleed.** `diff.rs:1108`:

```rust
.child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono)
    .text_color(hx.tx1).overflow_hidden().child(r.branch.clone())),
```

`overflow_hidden()` clips what a box paints *outside itself*; it does not stop
text from wrapping. With no `whitespace_nowrap()`, a branch like
`fix/project-row-element-id-collision` wraps to three lines. The child's content
height then exceeds its parent row, which is pinned to `h(px(34.0))` with
`items_center` — so the text overflows the row **symmetrically, above and
below**. That is why the first line appears above the header and the last line
lands on top of the row underneath.

**Row 2 — the collision.** `diff.rs:1110-1160` is a `justify_between` row of two
intrinsically-sized groups:

| Group | Contents | Approx. width |
|---|---|---|
| Left (`:1117`) | base button + `Changed files N` | ~207px |
| Right (`:1149`) | push control + view toggle + `+N` `−M` | ~160px |

Against 196px of usable width. The left group carries `min_w_0()`, but that is
inert: the base button is `flex_none()` and the `Changed files N` label has no
truncation, so the group's minimum content width never drops. The right group
has no `flex_none()`, so it is the one that gets squeezed, and its children
overflow back across the left group.

## Decision

**Let the second row wrap, and stop the first row from wrapping.**

Row 1's fix is not a preference — a fixed-height row containing wrapping text is
a bug at any width. Row 2 gets `flex_wrap()`, so that when the two groups cannot
share a line they stack instead of colliding.

### Why wrapping, over the alternatives

| Rejected | Why |
|---|---|
| Hide controls by priority as width drops (shrink the label to a bare number, drop the view toggle below `SPLIT_MIN_W` where Split is vetoed anyway) | Keeps one line at every width, but silently removes the toggle *and its tooltip* — which is the only thing that explains why splitting is unavailable. Also needs a new width threshold and the branching to go with it. |
| Raise the `diff_width` minimum from 220px to ~400px | One-line change, and admits the panel is barely usable at 220px — but it takes away the ability to shrink the panel to a sliver to see the editor, which is a real thing the drag handle is for. Fixes the symptom by banning the state. |

Wrapping needs no threshold, no new constant, and no state. Above ~400px the
header is byte-for-byte what it is today; below it, the groups stack. The
degradation is continuous rather than a cliff.

A useful property falls out of flexbox: `justify-content: space-between` with a
single item on a line degenerates to `flex-start`. So once wrapped, both lines
are left-aligned automatically — no `justify_start` override, no measuring.

## Design

### Row 1 (`diff.rs:1108`)

Replace `.overflow_hidden()` with `.truncate()`:

```rust
.child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono)
    .text_color(hx.tx1).truncate().child(r.branch.clone())),
```

`Styled::truncate()` is `overflow_hidden() + whitespace_nowrap() +
text_ellipsis()` (gpui `styled.rs:139`). The branch becomes one ellipsized line
and can no longer grow the row past its `h(px(34.0))`.

End-ellipsis rather than `text_ellipsis_start()`: branch names in this repo are
`type/subject`, so the leading segment is the part that identifies the work.

### Row 2 (`diff.rs:1110`)

On the row itself:

```rust
    .flex()
    .flex_none()
    .flex_wrap()      // groups stack rather than collide
    .items_center()
    .justify_between()
    .gap_x_2()        // between the groups when they share a line
    .gap_y_1()        // between the lines once wrapped
    .px_3()
    .py_2()
```

On the left group (`:1117`), add `.truncate()` to the `Changed files …` label.
`min_w_0()` on the group is already present and stays; without a truncatable
text child it cannot do anything, which is why the group never shrinks today.

On the right group (`:1149`), add `.min_w_0()` — deliberately **not**
`.flex_none()`.

`flex_none` was the first instinct: once the row wraps, the control group should
take a whole line rather than be compressed into illegibility. But pinning it
unshrinkable re-opens the very bug this fix exists to close. The width table
above measured the right group in its **unarmed** state. Armed, `render_push_
control` renders `push 3 → origin/<branch>?` (`diff.rs:883`, with `target`
defaulting to `origin/{branch}` at `:880`) — for a real branch name that is ~48
characters of `text_xs` mono, roughly 345px. Unshrinkable, that group hangs
straight off the panel's right edge.

So the group shrinks, and the shrinkage is aimed:

- The **push control** takes `.min_w_0().truncate()`. It is the one control here
  whose width is unbounded and data-dependent, so it is the one that gives
  ground.
- The **view toggle** already carries `flex_none()` internally (`diff.rs:967`)
  and keeps it. A squeezed toggle is unclickable.
- The **`+N` / `−M` counts** each get `.flex_none()`. A clipped number is not a
  smaller number, it is a wrong one.

The row also gets `.overflow_hidden()` as a hard backstop. Wrapping and aimed
shrinking should keep everything inside, but the failure being fixed is content
painting *over* its neighbours, and clipping is a strictly better way to lose
than that. The base menu is unaffected — it renders at the workspace root
(`workspace.rs:1748`), not inside this row.

Neither inner group gets `flex_wrap()`: each stays a single line and wraps as a
unit.

### Resulting behaviour

| Panel width | Header |
|---|---|
| ≥ ~400px | Unchanged from today: one line, base + count left, controls right |
| 220–400px | Two left-aligned lines: base + truncated count, then controls |
| Any width | Branch name is one ellipsized line, never overflowing its row |

## Testing

**No automated tests.** This is pure layout: there is no logic to extract into a
pure function, and the repo has no layout-test harness — nor is one worth
standing up for a styling change. Claiming otherwise would be the dishonest part
of this spec, so it is stated plainly here instead.

Verified by eye, in a real build, at the drag extremes:

1. Drag the diff panel to its **narrowest**. The branch name is one line ending
   in `…` and stays inside its row. The base button and file count sit on one
   line; the view toggle and `+N −M` sit on the line below. Nothing overlaps.
2. Widen slowly through ~400px. The two lines rejoin into one; the transition is
   a reflow, not a jump or a flicker.
3. At a **wide** panel, confirm the header is identical to before this change —
   base and count left, controls hard right.
4. Select a task with a **long** branch name and one with a **short** one. The
   short name shows no stray `…`.
5. With the panel narrow, confirm the base dropdown still opens on click and is
   positioned at the cursor — wrapping moves the button, and the menu is
   anchored to the click position (`diff.rs:944`), not to the button.
6. **Arm the push control at minimum width.** On a task with unpushed commits
   and a long branch name, click the push control so it reads
   `push N → origin/<branch>?`, then narrow the panel. The label ellipsizes; the
   view toggle and `+N −M` keep their size; nothing leaves the panel.

Checks 4 and 6 exist because the width table in this spec measured the header in
its **default** state only. Its variable-width states — an armed push, a long
compare-ref, the `(+n untracked not shown)` suffix — are where a layout fix of
this kind regresses, and none of them are the state you happen to be looking at
while writing it.

### Accepted tradeoff

`truncate()` on the file-count label means that at minimum width the string
`Changed files  3/12 reviewed  (+2 untracked not shown)` ellipsizes, and the
suffix is what disappears — the suffix that `diff.rs:1152`'s comment exists to
defend (*"Never imply the list is complete when it is not"*). This is accepted
rather than overlooked: the alternative is the overlap, which hides the count
*and* everything near it. The invariant degrades at one width instead of being
destroyed at that width. If it needs closing later, the fix is to lead with the
untracked marker rather than trail it — a string change, deliberately out of
scope for a layout fix.

## Scope

Out of scope:

- The `SPLIT_MIN_W` (720px) veto on split view. It is orthogonal and already
  behaves correctly — the toggle stays visible and explains itself by tooltip.
- The 220px `diff_width` minimum. Left as-is deliberately; see the rejected
  alternatives above.
- The file list and diff body below the header. They already handle narrow
  widths.
