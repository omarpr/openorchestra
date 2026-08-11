# Terminal File Drop Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a user drag one or more files from Finder onto an agent terminal pane and have the shell-quoted paths appear in the running agent's prompt.

**Architecture:** Zed's own file-drop path (`Item::handle_drop`) is unreachable here because `TerminalPane` deliberately constructs `TerminalView` with invalid `Project`/`Workspace` weak handles. So we attach gpui's `drag_over`/`on_drop` directly to the pane's own root `div()` — both live on `InteractiveElement`, so no element `.id()` is required — and paste the formatted paths into the pty ourselves. Path formatting is a free function so it can be unit-tested without a window or a pty.

**Tech Stack:** Rust, gpui (zed rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4`), zed's `terminal` / `terminal_view` crates, `shlex` 1.3.0.

## Global Constraints

- **Design spec:** `docs/superpowers/specs/2026-08-09-terminal-file-drop-design.md`. It is the authority; this plan implements it.
- **`crates/harness-app` is outside the root cargo workspace.** A root `cargo build` or `make test` **skips it and still reports success**. Every build/test command in this plan must be run from inside `crates/harness-app`.
- **All zed dependencies are pinned to rev `952d712dac48a4af2c54fb22c82d82a9d69b72d4`.** Do not add a zed dep on any other rev.
- **`crates/harness-app/Cargo.toml` comments every non-obvious dependency with *why it is there*.** Any new dep follows that convention.
- **Terminal pane only.** Do not add drop handling to the Editor, Chat, or Diff tabs.
- **Do not open a pull request** until the user has personally run the built app and confirmed the feature works. Task 3 is a hard stop.

---

### Task 1: `paths_to_prompt_text`

The pure path-formatting function, with its tests. Self-contained: no rendering, no drop wiring, nothing that needs a window.

**Files:**
- Modify: `crates/harness-app/Cargo.toml` (add `shlex`)
- Modify: `crates/harness-app/src/terminal_pane.rs` (add function + `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: `fn paths_to_prompt_text(paths: &[PathBuf]) -> Option<String>` — private to `terminal_pane.rs`. Task 2 calls it.

- [ ] **Step 1: Add the `shlex` dependency**

In `crates/harness-app/Cargo.toml`, append to the `[dependencies]` section:

```toml
# Shell-quoting for paths dropped onto a terminal pane. Same version zed's own
# `terminal_view` pins, so a dropped path is quoted here exactly as zed would
# quote it — the format users already expect from every other terminal.
shlex = "1.3.0"
```

- [ ] **Step 2: Write the failing tests**

Append to `crates/harness-app/src/terminal_pane.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_clean_path_is_unquoted_and_padded() {
        assert_eq!(
            paths_to_prompt_text(&[PathBuf::from("/Users/sorce/notes.md")]),
            Some(" /Users/sorce/notes.md ".to_string())
        );
    }

    #[test]
    fn screenshot_name_with_spaces_is_quoted_intact() {
        // The case this feature exists for: macOS names every screenshot with
        // spaces, which is exactly what makes typing the path by hand awful.
        let p = PathBuf::from("/Users/sorce/Desktop/Screenshot 2026-08-09 at 1.23.45 PM.png");
        assert_eq!(
            paths_to_prompt_text(&[p]),
            Some(
                " '/Users/sorce/Desktop/Screenshot 2026-08-09 at 1.23.45 PM.png' ".to_string()
            )
        );
    }

    #[test]
    fn multiple_paths_are_space_separated_and_quoted_only_where_needed() {
        let paths = [
            PathBuf::from("/a/one.png"),
            PathBuf::from("/b/two three.png"),
            PathBuf::from("/c/four.png"),
        ];
        assert_eq!(
            paths_to_prompt_text(&paths),
            Some(" /a/one.png '/b/two three.png' /c/four.png ".to_string())
        );
    }

    #[test]
    fn empty_input_yields_nothing() {
        assert_eq!(paths_to_prompt_text(&[]), None);
    }

    #[cfg(unix)]
    #[test]
    fn unrepresentable_paths_are_skipped_not_fatal() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        // Invalid UTF-8 — `to_str()` returns None for this.
        let bad = PathBuf::from(OsStr::from_bytes(&[0x2f, 0xff, 0xfe]));

        // A bad path must not take its good neighbours down with it.
        assert_eq!(
            paths_to_prompt_text(&[bad.clone(), PathBuf::from("/tmp/ok.png")]),
            Some(" /tmp/ok.png ".to_string())
        );
    }

    #[cfg(unix)]
    #[test]
    fn all_paths_unrepresentable_yields_none_not_a_bare_space() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let bad = PathBuf::from(OsStr::from_bytes(&[0x2f, 0xff, 0xfe]));
        // Some(" ") here would inject a stray space into the user's prompt.
        assert_eq!(paths_to_prompt_text(&[bad]), None);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

```bash
cd crates/harness-app && cargo test terminal_pane::tests
```

Expected: **compile error**, `cannot find function 'paths_to_prompt_text' in this scope`, once per call site.

(A compile failure is the correct "red" here — the function does not exist yet.)

Note the filter is `terminal_pane::tests`, the module path — **not** `paths_to_prompt_text`.
Cargo matches its filter against each test's full path (`terminal_pane::tests::<name>`),
and no test is named after the function under test, so filtering on the function name
silently matches zero tests and prints `test result: ok. 0 passed`. That reads as
success at both the red and the green step, proving nothing at either.

- [ ] **Step 4: Write the implementation**

Add to `crates/harness-app/src/terminal_pane.rs`, after the `impl Render for TerminalPane` block and before the test module:

```rust
/// Format dropped file paths for insertion into the running agent's prompt.
///
/// Shell-quoted only where a path needs it, joined by single spaces, with one
/// leading and one trailing space: the leading space keeps the first path from
/// fusing onto whatever is already at the cursor, and the trailing one lets you
/// keep typing straight after the drop. Byte-identical to what zed's
/// `TerminalView::add_paths_to_terminal` produces for any path both accept.
///
/// `None` — never `Some(" ")` — when nothing usable survived, so the caller
/// pastes nothing rather than injecting a stray space.
fn paths_to_prompt_text(paths: &[PathBuf]) -> Option<String> {
    let mut text = String::new();
    for path in paths {
        // A path we cannot represent is skipped and *named*. Zed's own version
        // filter_maps the same two failures away silently, so a five-file drop
        // quietly becomes four with nothing said about it.
        let Some(quoted) = path.to_str().and_then(|p| shlex::try_quote(p).ok()) else {
            log::warn!(
                "dropped path skipped (not shell-quotable): {}",
                path.display()
            );
            continue;
        };
        text.push(' ');
        text.push_str(&quoted);
    }
    if text.is_empty() {
        return None;
    }
    text.push(' ');
    Some(text)
}
```

- [ ] **Step 5: Run the tests to verify they pass**

```bash
cd crates/harness-app && cargo test terminal_pane::tests
```

Expected: `test result: ok. 6 passed` (4 on non-unix). If it says `0 passed`, you used
the wrong filter — see the note in Step 3.

If `screenshot_name_with_spaces_is_quoted_intact` fails on the quoting style, read the actual output before changing the assertion — `shlex::try_quote` uses single quotes, and the test asserts that deliberately.

- [ ] **Step 6: Verify the crate still builds clean**

```bash
cd crates/harness-app && cargo check
```

Expected: no warnings about an unused `paths_to_prompt_text` — it is used by the tests, so `cfg(test)` covers it. If you see `function is never used` on a plain `cargo check` (no `--tests`), that is expected at this task boundary and Task 2 resolves it. Do not silence it with `#[allow(dead_code)]`.

- [ ] **Step 7: Commit**

```bash
git add crates/harness-app/Cargo.toml crates/harness-app/Cargo.lock crates/harness-app/src/terminal_pane.rs
git commit -m "feat(terminal): format dropped file paths for the agent prompt"
```

---

### Task 2: Accept the drop

Wire gpui's drag/drop onto the pane and paste the formatted text into the pty.

**Files:**
- Modify: `crates/harness-app/src/terminal_pane.rs` (imports, `drop_paths` method, `render` body)

**Interfaces:**
- Consumes: `paths_to_prompt_text(&[PathBuf]) -> Option<String>` from Task 1.
- Produces: no new public surface. `TerminalPane::drop_paths` is private.

- [ ] **Step 1: Extend the imports**

In `crates/harness-app/src/terminal_pane.rs`, change the `gpui` import to add `ExternalPaths`:

```rust
use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, Focusable, IntoElement, Render,
    Styled, WeakEntity, Window, div, prelude::*, px,
};
```

and change the theme import to bring in the tint helper:

```rust
use crate::theme::{theme, tint};
```

- [ ] **Step 2: Add the `drop_paths` method**

Inside the existing `impl TerminalPane { … }` block, after `fn spawn`:

```rust
    /// Insert paths dropped from Finder into the running agent's prompt.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.view.clone() else { return };
        let Some(text) = paths_to_prompt_text(paths.paths()) else {
            return;
        };

        // Both handles are read out before the update: `view.read(cx)` and
        // `view.focus_handle(cx)` each hold an immutable borrow of `cx`, which
        // `terminal.update(cx, …)` and `window.focus(…, cx)` need mutably.
        let handle = view.focus_handle(cx);
        let terminal = view.read(cx).terminal().clone();

        // Focus follows the paste — the point of the drop is to keep typing.
        window.focus(&handle, cx);

        // `paste`, not `input`: this wraps the text in bracketed-paste markers
        // when the running TUI has enabled that mode (Claude Code does), which
        // is what keeps a multi-file drop from being read as a submit.
        terminal.update(cx, |terminal, _| terminal.paste(&text));
    }
```

- [ ] **Step 3: Wire the drop into `render`**

In `impl Render for TerminalPane`, replace the `Some(view)` arm:

```rust
            Some(view) => div().size_full().bg(hx.bg0).child(view.clone()),
```

with:

```rust
            Some(view) => {
                let accent = hx.accent;
                let drag_bg = tint(hx.accent, 0.08);
                div()
                    .size_full()
                    .bg(hx.bg0)
                    // Always laid out, painted the background colour, so
                    // `drag_over` below changes only the *colour*. Adding
                    // `border_1` on hover instead would shrink the content box
                    // by 2px mid-drag, resize the pty, and make the agent
                    // redraw its whole prompt under the user's cursor.
                    .border_1()
                    .border_color(hx.bg0)
                    .drag_over::<ExternalPaths>(move |style, _, _, _| {
                        style.border_color(accent).bg(drag_bg)
                    })
                    .on_drop(cx.listener(|pane, paths: &ExternalPaths, window, cx| {
                        pane.drop_paths(paths, window, cx);
                    }))
                    .child(view.clone())
            }
```

Leave the `None` arm exactly as it is. A pane whose terminal has not spawned gets no drop handlers, so dragging over it shows no highlight — which correctly says the drop will not land, rather than swallowing it.

- [ ] **Step 4: Build and confirm it compiles**

```bash
cd crates/harness-app && cargo check
```

Expected: clean. Two failures worth predicting:
- `cannot borrow *cx as mutable` in `drop_paths` — you collapsed `handle`/`terminal` back into the `window.focus` or `update` call. Keep them as separate `let` bindings.
- `no method named terminal` — `TerminalView::terminal()` is `pub` at `terminal_view.rs:866`; check the import, not the method.

- [ ] **Step 5: Run the full crate test suite**

```bash
cd crates/harness-app && cargo test
```

Expected: all tests pass, Task 1's six included. Nothing in this task adds tests — the drop wiring needs a window harness this repo does not have, and the spec records that as deliberate.

- [ ] **Step 6: Commit**

```bash
git add crates/harness-app/src/terminal_pane.rs
git commit -m "feat(terminal): accept dropped files onto the agent pane"
```

---

### Task 3: Build for the user, and STOP

**Files:** none modified.

**Interfaces:**
- Consumes: a working build from Tasks 1 and 2.
- Produces: `dist/openOrchestra.app`, and a verification verdict from the user.

- [ ] **Step 1: Build the app bundle**

```bash
make app
```

Expected: `dist/openOrchestra.app` exists. Use `make app`, not `make run` — the debug binary degrades native notifications, and an unrelated behavioural difference muddies the user's test.

- [ ] **Step 2: Confirm the bundle is actually fresh**

```bash
ls -ld dist/openOrchestra.app && date
```

Expected: the bundle's mtime is from this build, not a stale one from a previous session.

- [ ] **Step 3: Hand it to the user with the verification script**

Tell the user the build is ready at `dist/openOrchestra.app`, and give them these checks to run against a terminal tab running Claude Code:

1. Drag **one screenshot** over the terminal — the border turns accent and the background tints.
2. Release — the quoted path lands at the prompt and the terminal takes focus.
3. Drag **three files at once** — all three appear, space-separated, quoted only where needed.
4. Drag over the terminal, then release **outside the window** — nothing is pasted.
5. While the drag hovers, confirm the prompt does **not** reflow or redraw (the border-colour-only check).
6. Watch for stray mouse behaviour during step 1 — gpui synthesizes left-button `MouseMove` events during a drag. Claude Code does not enable mouse reporting (`terminal_pane.rs:160-165` documents the same fact about the scroll wheel), so nothing should happen, but this is the thing to catch.

- [ ] **Step 4: STOP. Wait for the user's verdict.**

**Do not proceed to Task 4 without an explicit confirmation from the user that they ran the app and it works.** If any check fails, fix it, rebuild, and hand it back — do not open a PR on a partial pass.

---

### Task 4: Open the pull request

Only after the user has confirmed Task 3.

**Files:** none modified.

**Interfaces:**
- Consumes: the user's explicit verification verdict from Task 3.
- Produces: a PR against `master`.

- [ ] **Step 1: Re-read the diff before publishing it**

```bash
git diff origin/master...HEAD
```

Check: no debug prints, no commented-out code, no stray `#[allow(...)]`, no unrelated files. The branch should contain exactly the spec commit, the plan commit, and Tasks 1–2.

- [ ] **Step 2: Confirm tests and build are green on the final tree**

```bash
cd crates/harness-app && cargo test && cargo check
```

Expected: both pass. Report the real output — if anything fails, stop and fix.

- [ ] **Step 3: Push the branch**

```bash
git push -u origin feat/terminal-file-drop
```

- [ ] **Step 4: Open the PR**

```bash
gh pr create --base master --title "feat(terminal): drag and drop files onto the agent pane" --body "$(cat <<'EOF'
## What

Drag one or more files from Finder onto a terminal pane and their shell-quoted
paths appear in the running agent's prompt. Screenshots were the motivating
case: macOS names them with spaces, which makes typing the path by hand
miserable.

## Why it did not already work

Zed's file-drop support lives in `Item::handle_drop`, which opens with
`let Some(project) = self.project.upgrade() else { return false }`. `TerminalPane`
constructs its `TerminalView` with `WeakEntity::<Project>::new_invalid()` — a
deliberate choice documented at `terminal_pane.rs:4-7`, and what lets this app
host a terminal without standing up a real zed `Workspace`. So that branch was
dead on its first line. `handle_drop` is also only reachable through a real
`Pane`, which this app does not have either.

The fix attaches gpui's `drag_over`/`on_drop` to the pane's own root element
instead of trying to revive zed's path. Both live on `InteractiveElement`, so no
element id is needed, and drops dispatch on a bubble-phase `MouseUpEvent` that
`TerminalElement`'s own handler does not stop.

## Notes for review

- Path formatting is a free function rather than a call to zed's `pub`
  `add_paths_to_terminal`, for two reasons: it is unit-testable without a window
  or a pty, and zed's `filter_map` silently discards unrepresentable paths where
  this names them in a `log::warn!`. Output is byte-identical for any path both
  accept.
- The pane's border is always laid out and merely repainted on drag. Adding a
  border on hover would shrink the content box by 2px, resize the pty, and make
  the agent redraw its prompt mid-drag.
- Terminal pane only. Editor, Chat, and Diff tabs are unchanged.

## Testing

Six unit tests on the formatter, including the macOS-screenshot-with-spaces
case and non-UTF-8 paths. The drop wiring itself is verified manually — see the
spec — because simulating a gpui drag needs a window harness this repo does not
have.

Manually verified on `dist/openOrchestra.app` against a terminal tab running
Claude Code: single drop, multi-file drop, release-outside-window, and no
prompt reflow while dragging.

Design: `docs/superpowers/specs/2026-08-09-terminal-file-drop-design.md`
Plan: `docs/superpowers/plans/2026-08-09-terminal-file-drop.md`
EOF
)"
```

- [ ] **Step 5: Report the PR URL to the user**
