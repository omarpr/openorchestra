# Design — Finish the native git/diff and preview surfaces in the hosted Zed editor

**Status:** Approved design · July 26, 2026
**Scope:** Four changes to `crates/harness-app`, all against the existing
`workspace_host.rs` / `shortcuts.rs` seam.
**Relationship to prior work:** completes Deliverable **B2** of
`docs/2026-07-22-main-diff-editor-design.md`. That deliverable added the `git_ui`
crate, called `git_ui::init(cx)`, and bound ⌘⇧D — but stopped at the crate
boundary. It never adopted the two *window-root duties* that make `git_ui`'s
surfaces actually appear. No architectural decision changes.

---

## Motivation

From the user: the embedded Zed editor still does not give you a diff viewer
"like Zed does." Three concrete gaps:

1. **No Git panel.** Zed's changed-files panel — stage/unstage, commit box — is
   absent from the editor tab.
2. **⌘⇧D does not open the project diff.** The binding exists but nothing
   usable appears.
3. **No Markdown preview "eye" button.** Zed puts one at the top-right of a
   `.md` editor; ours has the action bound to a chord but no affordance.

Gaps 1 and 2 are the same root cause (below). Gap 3 is separate and is Part 4.

---

## Diagnosis

`git_ui::init(cx)` (`main.rs:240`) *does* register the action side. It installs a
`cx.observe_new::<Workspace>` hook (zed `git_ui.rs:90`) which runs
`ProjectDiff::register`, `git_panel::register`, `staged_diff`/`unstaged_diff`/
`branch_diff` registration, the conflict views, and `editor::set_blame_renderer`.
Every one of those attaches to our hosted `Workspace` with no per-instance
wiring, exactly as the B2 design predicted.

What is missing is on the *host* side. `workspace_host.rs` already names this
category of work — its `init_toolbar` block is commented as "the FOURTH
window-root duty this embed adopts (after action wiring, key context, and the
modal layer)". Zed's app crate performs two further duties that we do not:

| Duty | Zed does it in | openOrchestra |
|---|---|---|
| Load + dock `GitPanel` | `zed.rs:760,784` (`initialize_panels`) | never — only `ProjectPanel` (`workspace_host.rs:222-257`) |
| Install the git/diff pane toolbars | `zed.rs:1420-1437` (`initialize_pane`) | never — `init_toolbar` (`:184-200`) adds only the two search bars |

Registering an action that deploys a view is not the same as providing the
surface that view renders into. This is the same failure the file already
documents for search: without `ProjectSearchBar`, ⌘⇧F "deploys a results view
with NO query bar — nothing to type into, which reads as *search is broken*."

The July 22 plan's Task 6 confirms the omission: its steps are add-crate,
re-diff `[patch]`, call `init`, bind a chord. Neither the panel nor the toolbars
appear anywhere in it.

---

## Part 1 — Diagnose ⌘⇧D before changing it

We do not yet know *which* failure ⌘⇧D is. Static reading rules out a chord
collision (`cmd-shift-d` is bound exactly once, `shortcuts.rs:210`, and the
catalog's own `no_duplicate_keys_within_a_scope` test enforces it) and rules out
a conditional bail — `ProjectDiff::deploy_at` calls `add_item_to_active_pane`
unconditionally. Four candidates remain, and they need different fixes.

### Approach — probe BOTH layers, not just the action

Extend the existing `probe_keys` harness (`workspace_host.rs:549`, gated by
`HARNESS_PROBE_KEYS`) to fire ⌘⇧D twice:

1. as a **real chord**, via `press` → `window.dispatch_keystroke` — the actual
   keymap, with context matching, precedence, and the catalog binding;
2. as a **bare action**, via `focused_dispatch` → `window.dispatch_action`,
   which bypasses the keymap entirely and speaks straight to the handler.

Probing only the action would be a trap: it reports success for a chord that
never matches anything, so a broken *binding* would be misread as a working one
and send the fix to the wrong layer. Comparing the two is what separates them:

| chord | action | Meaning |
|---|---|---|
| +1 | — | works; nothing wrong at this layer |
| +0 | +1 | handler fine, the **binding** never matches (key-context / focus) |
| +0 | +0 | `git_ui`'s `observe_new` never registered the handler here |
| +1/+1, but `repo == None` | | opened against a `Project` with no git repository — the diff is legitimately empty, and the bug is in `Project` setup, not `git_ui` |

`active_repository` is read **after** the dispatches, not before:
`find_or_create_worktree` is fire-and-forget in `project_for`, so the git store
is not populated the instant the workspace exists, and an early read would
report `None` for timing reasons rather than real ones.

`probe_keys` already solves the other hard part — it re-focuses the hosted
workspace before each dispatch and waits a frame, because `dispatch_action`
resolves against the last *rendered* frame. So this is a new probe in an
established harness, not new machinery, and it is state-based, so it answers
over ssh.

**This runs before any ⌘⇧D wiring changes.** Fixing the wrong layer here is
cheap to do and expensive to detect.

---

## Part 2 — Dock the Git panel

Mirror the existing `ProjectPanel` block: `GitPanel::load(handle, cx)` →
`add_panel` → explicitly `set_open(true)` and `activate_panel(index)`.

The explicit open/activate is **load-bearing and non-obvious**, and the file
already carries the scar tissue: `Workspace::add_panel` registers the panel but
leaves the dock closed *and* `active_panel_index` at `None`, while `Dock::render`
bails unless the dock is both open and has an active panel. Zed only activates
on its panel-move path, so out-of-tree callers must do it themselves
(`workspace_host.rs:233-239`).

Two properties make this land without disturbing what already works:

- **`GitPanel` defaults to the right dock** (`"dock": "right"`, zed
  `assets/settings/default.json:962`). It therefore does not compete with the
  file tree for the left dock, and cannot steal the left dock's active-panel
  slot — which the current `activate_panel(panels_len - 1)` call would otherwise
  be vulnerable to. The helper resolves the dock from the panel's own
  `position()` rather than assuming, so this stays true if the setting changes.
- **Reachability when closed:** add a `Scope::Workspace` binding for the git
  panel's `ToggleFocus` action (⌘⇧G, matching Zed's own chord).

### Docked, but starting closed

The panel is added and **activated** but its dock is left **closed**. Forcing it
open would spend a third of the editor's width on every tab, in a window that
already gives two columns to the harness chrome — and zed does not open it by
default either.

Activation still happens unconditionally, and that split is the subtle part:
a dock with no active panel renders nothing *even once opened*, so a
start-closed panel must claim the slot at load time or ⌘⇧G later opens onto
blank space — precisely the bug this helper exists to prevent. Hence
`activate_panel` always, `set_open` only for `StartOpen::Yes`.

### Refactor

`workspace_for` would then hold two near-identical async panel blocks. Extract
one `add_panel_when_ready` helper — the same shape Zed uses (`zed.rs:766`) —
and route both `ProjectPanel` and `GitPanel` through it. This is targeted at the
code being touched, not opportunistic refactoring.

### Probe

Extend `probe` (`:276`) to assert the right dock the way it already asserts the
left: panel registered, dock open, active panel present. The existing doc comment
on `probe` explains why that specific triple is the assertion that matters.

---

## Part 3 — Install the git/diff pane toolbars

Add `ProjectDiffToolbar` to `init_toolbar`, plus `StagedDiffToolbar`,
`UnstagedDiffToolbar`, and `BranchDiffToolbar` — these render the stage-all and
commit affordances in the diff view's header. Zed constructs them in
`initialize_pane` (`zed.rs:1420-1427`).

`init_toolbar` is already invoked both for the panes present at construction and
for every later `Event::PaneAdded` (`workspace_host.rs:201-219`), so new toolbar
items inherit that coverage with no extra wiring.

The zed-specific extras stay out, consistent with the existing rule in that
block: quick actions, diagnostics, and log views "live in zed's app crate and
are not part of *an editor that can search*." Same test applies here — we add
what the diff views need to function, nothing more.

---

## Part 4 — The Markdown preview "eye" button

Zed shows an eye icon at the top-right of an editor when the active buffer is
Markdown; clicking it opens the rendered preview (alt-click opens it in a
split). We have the *action* — `markdown_preview::init(cx)` runs (`main.rs:243`)
and ⌘⇧M / ⌘⌥M are bound (`shortcuts.rs:217-228`) — but no button.

### This one cannot be reused

Unlike `git_ui` and `markdown_preview`, the button is **not in a library
crate**. It lives at `crates/zed/src/zed/quick_action_bar/preview.rs`, a module
inside zed's *binary* crate (`zed/Cargo.toml`, `[[bin]] name = "zed"`), and at
this rev `quick_action_bar` is not a standalone crate. There is nothing to add
to `Cargo.toml`. It must be reimplemented — about 60 lines.

That is acceptable because every API it depends on is public and already in our
dependency cone:

| Need | Source | Status |
|---|---|---|
| "is the active item a Markdown editor?" | `MarkdownPreviewView::resolve_active_item_as_markdown_editor` (`markdown_preview_view.rs:198`) | `pub`; crate already a dep |
| Toolbar item trait | `workspace::ToolbarItemView` (`toolbar.rs:24`) | already a dep |
| Eye icon | `IconName::Eye` (`icons/src/icons.rs:122`), re-exported via `ui` prelude | needs `ui` added as a **direct** dep |
| Actions to dispatch | `::markdown_preview::{OpenPreview, OpenPreviewToTheSide}` | already bound to ⌘⇧M / ⌘⌥M |

`ui` is already compiled as part of the cone (`editor` and `workspace` both
depend on it), so adding it costs a Cargo line, not build time.

### Shape

A focused new module — `crates/harness-app/src/markdown_preview_button.rs` —
holding a `WeakEntity<Workspace>` and implementing `ToolbarItemView`:

- `set_active_pane_item` returns `ToolbarItemLocation::PrimaryRight` (Zed's
  placement) and calls `cx.notify()`, so the button appears and disappears as
  you move between tabs.
- `render` resolves the active item through the workspace; returns an empty
  element unless it is a Markdown editor.
- Click dispatches `OpenPreview`; alt-click dispatches `OpenPreviewToTheSide`,
  matching Zed's behaviour and its tooltip.

Registered in `init_toolbar` alongside the Part 3 git/diff toolbars, so all
toolbar additions land in one place and inherit the existing `Event::PaneAdded`
coverage.

### Naming hazard

This crate has its **own** `markdown_preview` module (the diff-panel renderer),
which shadows zed's crate — `main.rs:243` already works around this by writing
`::markdown_preview::init(cx)` with a leading `::`. The new module must use the
same fully-qualified form. Without it the path silently resolves to the local
module.

### Scope

Markdown only. Zed's implementation also covers SVG and CSV (the latter behind
a feature flag); the user asked for `.md`, and the `PreviewType`-style branching
is not worth carrying for surfaces we are not shipping.

---

## Explicitly out of scope

- **The custom diff/review panel stays.** `diff_review.rs` and
  `render_diff_panel` carry the review-comment → hand-back-to-agent flow and the
  per-file reviewed checkboxes, which `git_ui` has no equivalent for. This work
  is purely additive, matching B2's original framing.
- **Status-bar items.** Zed's `initialize_pane` also adds a git-blame status
  item and a merge-conflict indicator to the workspace status bar
  (`zed.rs:611-613`). Our hosted workspace runs no status bar items at all;
  adding one is a separate decision, not part of "the diff viewer works."
- **Gutter change-bars** are already expected to work via the editor reading
  `BufferDiff` off the hosted `Project`'s git store. If Part 1 reports
  `active_repository == None`, that assumption is wrong and becomes its own
  finding — recorded, not silently fixed here.

---

## Testing strategy

No new backend logic, so nothing here is headless-unit-testable — this is
GPUI view wiring, the same category the repo already handles with probes.

- **Gate:** `cargo check` and `cargo clippy` **run from `crates/harness-app`**.
  The root cargo workspace does not build this crate, so a root-level green is
  false.
- **Part 1:** the `HARNESS_PROBE_KEYS` run reports the three-way split above.
- **Part 2:** the extended `probe` asserts the right dock's (panels, open,
  active) triple.
- **Part 3:** confirmed in the app — a toolbar that renders is not observable
  from state alone. `probe`'s own doc comment already concedes this limit:
  "an element correct in every respect but sized to zero still logs clean here."
- **Part 4:** the *predicate* is testable without a window — given an active
  item, does the button decide to show? — but it needs a `Workspace`, so it
  rides the probe harness too: open a `.md` file via `HARNESS_PROBE_OPEN` and
  log whether `resolve_active_item_as_markdown_editor` returns `Some`, then
  repeat against a non-Markdown file to prove it correctly returns `None`.
  Visual confirmation of the icon itself is in-app.

The backend crates stay UI-free and fully tested; the tree stays clippy-clean.

---

## Risks

| Risk | Mitigation |
|---|---|
| Fixing the wrong layer for ⌘⇧D | Part 1 diagnoses before Part 2/3 change anything |
| Git panel steals the left dock's active slot from the file tree | `GitPanel` docks right by default; activate per-dock, never `panels_len - 1` on a shared dock |
| Cold build cost | No `target/` in this worktree; the editor cone is a ~6–7 GB cold build. Accepted, and the first `cargo check` is budgeted as slow |
| `git_ui` toolbar types not exported out of tree | Verified against the pinned rev before writing code; a private type is a design-changing finding, not a compile-and-see |
| Part 4's `markdown_preview` path silently resolving to this crate's own module of that name | Use the leading-`::` form throughout, as `main.rs:243` already does |
| Reimplemented eye button drifts from Zed's as the pin moves | It depends only on public API (`resolve_active_item_as_markdown_editor`, `ToolbarItemView`); a rev bump that breaks it breaks it at compile time, not silently |
