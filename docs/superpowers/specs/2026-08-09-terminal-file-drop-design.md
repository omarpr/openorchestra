# Drag-and-drop files onto the agent terminal

**Date:** 2026-08-09
**Status:** approved (design)

## Problem

Dragging a file from Finder onto a terminal tab does nothing. Drop three
screenshots on a pane running Claude Code and the cursor snaps back to Finder;
the prompt is untouched and nothing says why. The workaround is to find each
file's path by hand and type it, which for a macOS screenshot means
transcribing `Screenshot 2026-08-09 at 1.23.45 PM.png` — a name built to defeat
transcription.

Every terminal the user has trained on does this. Superset does it. The absence
reads as a bug, not a missing feature.

### Why it does nothing today

Not a misconfiguration. Zed's file-drop support lives in `Item::handle_drop`
(`terminal_view.rs:1572`), whose first statement is:

```rust
let Some(project) = self.project.upgrade() else { return false };
```

`TerminalPane` constructs its `TerminalView` with
`WeakEntity::<Project>::new_invalid()` and
`WeakEntity::<Workspace>::new_invalid()`. That is deliberate and documented at
`terminal_pane.rs:4-7`: `TerminalView` only touches those handles on graceful
`.upgrade().ok()` paths, which is what lets this app host a terminal without
standing up a real zed `Workspace`. The cost is that every code path gated on a
live `Project` — including this one — is dead here. `handle_drop` returns
`false` on its first line, always.

`handle_drop` is also reached only through a real `Pane`, which this app does
not have either. Two independent reasons the upstream path cannot fire.

### Why the fix is nonetheless small

The plumbing beneath the dead branch is intact:

- `gpui_macos` registers the window for dragged types.
- `window.rs:4600` translates `FileDropEvent::Entered` into an `active_drag`
  whose value is an `ExternalPaths`.
- `drag_over` and `on_drop` are declared on `InteractiveElement`
  (`div.rs:1097`, `div.rs:1133`), **not** `StatefulInteractiveElement` — so a
  plain `div()` accepts them with no `.id()`.
- Drops dispatch on a bubble-phase synthesized `MouseUpEvent` guarded by
  `hitbox.is_hovered()` (`div.rs:2645`). `TerminalElement`'s own mouse-up
  handler (`terminal_element.rs:759`, via `generic_button_handler`) never calls
  `stop_propagation()`, so the event reaches an ancestor's drop listener.

So the work is to attach a drop listener to the pane's own root element, rather
than to revive zed's.

## Decision

**Accept external file drops on the terminal pane only, and insert the dropped
paths into the running agent's prompt as shell-quoted text.**

Three decisions inside that, each with a rejected alternative:

| Decision | Rejected alternative |
|---|---|
| Terminal pane only | Also the Editor tab (the hosted zed `Workspace` has its own drop contract to satisfy); or window-wide routing to the active tab (steals drops from tab reordering, which already uses `on_drop` at `center.rs:354`) |
| Quote only when needed — bare path when safe, single-quoted when it contains spaces | Never quote (a multi-file drop becomes ambiguous: a space could separate paths or sit inside one); one path per line (a newline may submit the turn early on agents that do not treat bracketed-paste newlines as literal) |
| Our own formatter, unit-tested | Call zed's `pub fn add_paths_to_terminal` — no code at all, but untestable from here and its `filter_map` silently discards unrepresentable paths |

### On writing our own formatter

`TerminalView::add_paths_to_terminal` (`terminal_view.rs:959`) is `pub` and does
exactly the right thing for well-behaved paths. It is still not what this design
uses, for two reasons:

1. **Silence on rejection.** Its body is a `filter_map` over
   `shlex::try_quote(path.to_str()?)`. A path that is not valid UTF-8, or that
   shlex declines to quote, is dropped from the output with nothing logged. Drop
   five files, receive four, learn nothing.
2. **Untestable from here.** Exercising it requires a live `TerminalView`, a
   window, and a pty. A free function over `&[PathBuf]` is a unit test.

The output is byte-identical to zed's for every path that both accept. This is
not a divergence in behaviour, only in observability and testability.

## Design

### `paths_to_prompt_text`

A free function in `terminal_pane.rs`:

```rust
/// Format dropped paths for insertion into the running agent's prompt.
/// `None` when nothing usable survived, so we never paste a lone space.
fn paths_to_prompt_text(paths: &[PathBuf]) -> Option<String>
```

Behaviour:

- Each path is rendered as `shlex::try_quote(path.to_str()?)`. `try_quote`
  returns the input unchanged when it needs no quoting, so ordinary paths stay
  readable and only the awkward ones acquire quotes.
- Rendered paths are joined by a single space, with **one leading and one
  trailing space**. The leading space keeps the first path from fusing onto
  text already at the cursor; the trailing one lets the user keep typing
  immediately after the drop. This matches zed's output exactly.
- A path that fails `to_str()` or `try_quote` is skipped and named in a
  `log::warn!`.
- Returns `None` for an empty input slice, and for the case where every path was
  skipped — so the caller pastes nothing rather than a bare `" "`.

`shlex` is a new dependency of `harness-app`; `terminal_view` already depends on
it, so it is present in the lockfile's graph.

### `TerminalPane::drop_paths`

```rust
fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>)
```

1. Return early if `self.view` is `None` or `paths_to_prompt_text` yields
   `None`.
2. Clone the `Entity<Terminal>` out of the view **before** updating it —
   `view.read(cx).terminal().clone()` — because reading and updating in one
   expression holds an immutable borrow of `cx` across a call needing `&mut`.
3. Focus the terminal, so the drop leaves the keyboard where the pasted text
   went.
4. `terminal.update(cx, |t, _| t.paste(&text))`.

**`paste`, not `input`.** `Terminal::paste` wraps its argument in
bracketed-paste markers when the running program has enabled that mode, which
Claude Code does. Without it a multi-file drop risks being read as a submit.

### Render wiring

In `TerminalPane::render`, on the `Some(view)` branch only:

```rust
div()
    .size_full()
    .bg(hx.bg0)
    .border_2()
    .border_color(hx.bg0)                // present, but invisible
    .drag_over::<ExternalPaths>(move |style, _, _, _| {
        style.border_color(accent)       // colour only
    })
    .on_drop(cx.listener(Self::drop_paths))
    .child(view.clone())
```

The border is **always laid out** and merely painted the background colour, so
`drag_over` changes colour and nothing else. Adding `border_1` on hover instead
would shrink the content box by 2px mid-drag, resize the pty, and make the agent
redraw its entire prompt under the user's cursor.

**The border is the entire affordance, and a background tint is not available.**
An earlier revision of this design added `.bg(tint(hx.accent, 0.08))` to the
`drag_over` style. That is dead paint. `TerminalView` wraps its content in
`div().size_full().bg(cx.theme().colors().editor_background)`
(`terminal_view.rs:1393`), `TerminalElement::paint` fills the bounds again
(`terminal_element.rs:1338`), and this app maps both colours through
`harness_zed_theme::hex`, which emits `#rrggbb` with **no alpha**
(`harness_zed_theme.rs:104,112,149`). Two opaque fills cover the whole content
box, so anything painted behind the terminal is invisible.

Hence `border_2()` rather than `border_1()`: with the wash unavailable, the
border carries the affordance alone and needs the weight. It is still laid out
at all times and only recoloured, so the no-reflow property is unchanged.

Painting a real wash would mean an absolutely-positioned overlay child drawn
*after* the terminal, which means tracking drag-hover in `TerminalPane` state
instead of in `drag_over` — materially more code for a drop target the border
already communicates. Not worth it.

The `None` branch — terminal still spawning, or failed to spawn — gets no drop
handlers. Dragging over it shows no highlight, which correctly signals that the
drop will not land, rather than accepting it and discarding it.

### Data flow

```
Finder drag enters window
  → gpui_macos → FileDropEvent::Entered
  → window.rs:4600 installs active_drag { value: ExternalPaths }
  → pane div's hitbox hovered → drag_over restyles (accent border + tint)
  → release → FileDropEvent::Submit → synthesized bubble-phase MouseUpEvent
  → TerminalElement's handler runs, does not stop propagation
  → pane div's on_drop → drop_paths → paths_to_prompt_text → terminal.paste
```

## Error handling

| Condition | Behaviour |
|---|---|
| Path is not valid UTF-8, or shlex refuses it | Skipped, named in `log::warn!`; the rest are still pasted |
| Every path rejected, or empty slice | Nothing pasted, no stray space |
| Terminal still spawning (`view: None`) | No drop handlers attached; no highlight, no drop |
| Terminal process has exited | `Terminal::paste` is already a no-op; the pane respawns a shell on its own |

## Testing

Unit tests on `paths_to_prompt_text`, in the same shape as the existing suites
in `tmux.rs`, `login_env.rs`, and `shortcuts.rs`:

| Case | Expectation |
|---|---|
| One clean path | `" /Users/example/notes.md "` |
| macOS screenshot name (spaces) | Single-quoted, spaces preserved |
| Three paths, mixed | Quoted only where required, single-space separated |
| Empty slice | `None` |
| Non-UTF-8 path (`OsStr::from_bytes`, unix) | Skipped |
| Every path invalid | `None`, not `Some(" ")` |

**Not unit-tested:** the drop wiring. Simulating a gpui drag needs a window
harness this repo does not have, and such a test would mostly assert that gpui
works. It is covered by manual verification instead.

### Manual verification (gate)

`make app`, then on `dist/openOrchestra.app`, with a terminal tab running Claude
Code:

1. Drag one screenshot over the terminal — the border turns accent. (Border
   only; see above for why there is no background tint.)
2. Release — the quoted path appears at the prompt, terminal takes focus.
3. Drag three files at once — all three appear, space-separated, quoted only
   where needed.
4. Drag over the terminal and release **outside** the window — nothing pasted.
5. Confirm the prompt does not reflow or redraw while the drag hovers (the
   border-colour-only check).
6. **The keyboard-modality case.** Type into the terminal, do *not* move the
   mouse, then drag a file in. Watch whether the border highlights.

### The one known hole, and why step 6 exists

`HitboxId::is_hovered` short-circuits to `false` whenever
`window.last_input_was_keyboard()` (`gpui/src/window.rs:629-641`). Input
modality is set from the raw `PlatformInput` (`window.rs:4542-4547`), where only
`KeyDown`, `MouseMove`, and `MouseDown` change it — `PlatformInput::FileDrop`
falls through untouched. On macOS an external drag emits *only* `FileDrop`
(`gpui_macos/src/window.rs:2908-2939`), never a synthetic `MouseMove` that would
flip modality back.

So if the last input to the window was a keystroke and the pointer has not
crossed the window since, the drop listener's `hitbox.is_hovered(window)` guard
fails and the drop does nothing.

Three things make this acceptable rather than blocking:

- It is framework behaviour. Zed's own drop targets have it identically; it is
  not introduced by this design, and `is_hovered_ignoring_last_input`
  (`window.rs:644`) is `pub(crate)`, so it cannot be worked around from here.
- **The failure is visible, not silent.** `drag_over` styling is gated on the
  same hitbox check (`div.rs:3181-3184`), so when the drop would not land, the
  border does not highlight. The affordance and the behaviour agree.
- In the realistic flow — alt-tab to Finder, pick up a file, drag it back — the
  pointer crosses the window and modality returns to `Mouse` before the drop.

Step 6 exists to measure how reachable it is in practice, which cannot be
settled by reading the source.

**Already closed upstream:** gpui synthesizes left-button `MouseMove` events
during a drag, which a TUI with mouse reporting would see as drag-select.
`terminal_element.rs:735` guards that path with `!cx.has_active_drag()`, so
there is nothing to check here.

## Scope

Out of scope, deliberately:

- Dropping onto the Editor, Chat, or Diff tabs.
- Copying dropped files into the worktree. Paths are passed by reference; the
  agent reads them where they sit.
- Any agent-specific syntax (`@file` and the like). Plain paths work across
  every agent the harness spawns.
