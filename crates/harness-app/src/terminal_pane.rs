//! Terminal pane backed by zed's `terminal_view::TerminalView` (proper pty
//! sizing, keyboard input, IME, scrollback, and selection) instead of a custom
//! renderer.
//!
//! TerminalView needs a `WeakEntity<Workspace>` and `WeakEntity<Project>`, but
//! only uses them on graceful (`.upgrade().ok()`) paths — never on construction
//! or render — so we pass invalid handles and never create a real Workspace.
//!
//! The consequence, which anything added here has to reckon with: every zed code
//! path gated on a live `Project` or `Workspace` is **dead** in this pane. It
//! does not misbehave, it never runs. File drop was the first feature to hit
//! that — zed's `Item::handle_drop` opens with `project.upgrade()` and returns
//! `false` — so this pane wires its own `drag_over`/`on_drop` (see
//! [`TerminalPane::drop_paths`]) rather than inheriting zed's. Expect to
//! re-implement, not to configure.
//!
//! Dropping files from Finder onto the pane inserts their shell-quoted paths
//! into whatever is running — the point being screenshots, whose macOS filenames
//! are miserable to type by hand.
//!
//! On creation the pane runs **whichever agent the worktree records**
//! (§4.5) — pi, opencode, claude, codex, kimi, grok — or a plain shell on the
//! read-only `main` entry (D8), with cwd = the configured root. When the
//! process exits (e.g. you quit the agent) the pane respawns a plain shell in
//! the same dir, so the tab stays usable.
//!
//! The command is not resolved here: an [`AgentCommand`] comes from the
//! worktree's `AgentRuntime` adapter, which has already run `on_spawn` to drop
//! in whatever config that agent needs to expose a status signal. A `None`
//! command means the CLI is not installed — a plain shell, not an error, since
//! a terminal is still useful.

use std::path::PathBuf;

use agent_runtime::AgentCommand;

use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, Focusable, IntoElement,
    KeyDownEvent, Keystroke, MouseDownEvent, Render, ScrollEvent, Styled, Task, WeakEntity, Window,
    div, prelude::*, px,
};
use project::Project;
use settings::Settings;
use terminal::{
    Event as TerminalEvent, TerminalBuilder,
    terminal_settings::{AlternateScroll, CursorShape as SettingsCursorShape, TerminalSettings},
};
use terminal_view::TerminalView;
use util::paths::PathStyle;
use util::shell::Shell;
use workspace::Workspace;

use crate::theme::theme;

/// A tmux session this pane should run under. `attach_only` restores a live
/// session (bare `tmux attach`); otherwise the session is created/attached with
/// its command.
pub struct TerminalSession {
    pub name: String,
    pub attach_only: bool,
    /// `HARNESS_*` for this worktree/task, handed to `new-session -e` so the
    /// pane's shell and any hook script inherit them. Empty when the pane has
    /// no task context (or on `attach_only`, where it cannot apply).
    pub env: Vec<(String, String)>,
}

pub struct TerminalPane {
    view: Option<Entity<TerminalView>>,
    /// Human-readable description of what's running (status bar chrome).
    pub spawned: String,
    error: Option<String>,
    worktree_root: PathBuf,
    window_id: u64,
    respawn_pending: bool,
    tmux_backed: bool,
    focus_handle: FocusHandle,
    /// tmux session this pane is a client of, for copy-mode handling.
    tmux_session: Option<String>,
    /// The pane is in tmux copy mode (entered by scrolling up or dragging a
    /// selection), where typed keys drive copy-mode shortcuts instead of
    /// reaching the agent. Polled from tmux after scroll/click; see
    /// [`TerminalPane::watch_copy_mode`].
    copy_mode: bool,
    watching_copy_mode: bool,
    /// Keys typed while copy mode is being cancelled, replayed in order after.
    held_keys: Vec<Keystroke>,
    cancelling_copy_mode: bool,
}

impl TerminalPane {
    pub(crate) fn direct_agent_exited(&self) -> bool {
        !self.tmux_backed
            && (self.respawn_pending || self.spawned == "$SHELL" || self.error.is_some())
    }

    /// `agent` is the resolved command for this worktree's agent, or `None` for
    /// a plain shell (the `main` entry, or an agent that is not installed).
    pub fn new(
        worktree_root: PathBuf,
        agent: Option<AgentCommand>,
        session: Option<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let window_id = window.window_handle().window_id().as_u64();
        let mut this = Self {
            view: None,
            spawned: String::new(),
            error: None,
            worktree_root,
            window_id,
            respawn_pending: false,
            tmux_backed: session.is_some(),
            focus_handle: cx.focus_handle(),
            tmux_session: session.as_ref().map(|s| s.name.clone()),
            copy_mode: false,
            watching_copy_mode: false,
            held_keys: Vec::new(),
            cancelling_copy_mode: false,
        };
        this.spawn(agent, session, window, cx).detach();
        this
    }

    /// Construct a pane in its spawning state without launching the process yet,
    /// for async command preparation off the UI thread.
    pub fn new_pending(
        worktree_root: PathBuf,
        label: String,
        session_name: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let window_id = window.window_handle().window_id().as_u64();
        Self {
            view: None,
            spawned: label,
            error: None,
            worktree_root,
            window_id,
            respawn_pending: false,
            tmux_backed: session_name.is_some(),
            focus_handle: cx.focus_handle(),
            tmux_session: session_name,
            copy_mode: false,
            watching_copy_mode: false,
            held_keys: Vec::new(),
            cancelling_copy_mode: false,
        }
    }

    /// Launch the resolved command in a pane created with [`Self::new_pending`].
    pub fn launch(
        &mut self,
        agent: Option<AgentCommand>,
        session: Option<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        self.tmux_backed = session.is_some();
        self.tmux_session = session.as_ref().map(|s| s.name.clone());
        let task = self.spawn(agent, session, window, cx);
        cx.notify();
        task
    }

    fn spawn(
        &mut self,
        agent: Option<AgentCommand>,
        session: Option<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        // No pty-level env on any path: every agent invocation is a shell line
        // with its env inlined as a command prefix, which both survives the trip
        // through tmux and takes precedence over the user's rc files.
        let (shell, spawned) = match session {
            // tmux-backed: run a tmux client; the agent and its env live in the
            // tmux server.
            Some(mut sess) => {
                let label = agent
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|| "shell".to_string());
                if agent.is_some() && !sess.attach_only {
                    sess.env
                        .push(("HARNESS_REMOTE_SESSION".into(), sess.name.clone()));
                }
                let args = crate::tmux::client_args(
                    &sess.name,
                    &self.worktree_root,
                    agent.as_ref().filter(|_| !sess.attach_only),
                    sess.attach_only,
                    &sess.env,
                );
                (
                    Shell::WithArguments {
                        program: crate::tmux::program(),
                        args,
                        title_override: Some(label.clone()),
                    },
                    format!(
                        "{label} ({} {})",
                        crate::platform::CURRENT.sessions.host,
                        sess.name
                    ),
                )
            }
            // No tmux: direct spawn. Still routed through the user's login
            // interactive shell rather than exec'ing the binary, so the agent
            // sees the same environment it would in a terminal — `.zshrc`
            // exports included. Its own env rides along inlined in the command
            // line (as on the tmux path), where it takes precedence over
            // anything the rc files set.
            None => match agent {
                Some(cmd) => {
                    let spawned = format!("{} ({})", cmd.label, cmd.program.display());
                    let (program, args) = worktrees::interactive_login_command(
                        &crate::tmux::agent_command_line(&cmd),
                    );
                    (
                        Shell::WithArguments {
                            program,
                            args,
                            title_override: Some(cmd.label),
                        },
                        spawned,
                    )
                }
                None => (Shell::System, "$SHELL".to_string()),
            },
        };

        self.spawned = spawned;
        self.error = None;
        self.view = None;

        let builder_task = TerminalBuilder::new(
            Some(self.worktree_root.clone()),
            terminal::TerminalMode::interactive(),
            shell,
            Default::default(),
            SettingsCursorShape::default(),
            // Off, not On: with alternate-scroll on, spinning the wheel inside a
            // full-screen TUI (Claude Code, pi, vim) that hasn't enabled mouse
            // reporting makes the terminal translate the wheel into arrow-key
            // presses (ESC O A / ESC O B), which the agent reads as navigation.
            // Off unsets that mode so the wheel is inert there; TUIs that *do*
            // enable mouse reporting still receive real scroll events (the
            // mouse-mode branch in Terminal::scroll_wheel runs first).
            AlternateScroll::Off,
            None,
            Vec::new(),
            std::time::Duration::ZERO,
            false,
            self.window_id,
            cx,
            Vec::new(),
            PathStyle::local(),
        );

        cx.spawn_in(window, async move |pane, cx| {
            let built = builder_task.await;
            pane.update_in(cx, |pane, window, cx| match built {
                Ok(builder) => {
                    let had_focus = pane.focus_handle(cx).is_focused(window);
                    let terminal = cx.new(|cx| builder.subscribe(cx));
                    // Respawn a shell when the process exits (don't leave a dead pane).
                    cx.subscribe(&terminal, |pane, _term, event: &TerminalEvent, cx| {
                        if matches!(event, TerminalEvent::CloseTerminal) && !pane.tmux_backed {
                            pane.respawn_pending = true;
                            cx.notify();
                        }
                    })
                    .detach();

                    let view = cx.new(|cx| {
                        TerminalView::new(
                            terminal,
                            WeakEntity::<Workspace>::new_invalid(),
                            None,
                            WeakEntity::<Project>::new_invalid(),
                            window,
                            cx,
                        )
                    });
                    let handle = view.focus_handle(cx);
                    pane.view = Some(view);
                    if had_focus {
                        window.focus(&handle, cx);
                    }
                    cx.notify();
                    true
                }
                Err(error) => {
                    pane.error = Some(format!("failed to spawn terminal: {error:#}"));
                    cx.notify();
                    false
                }
            })
            .unwrap_or(false)
        })
    }

    /// After a scroll or click, follow tmux's copy-mode state until the pane
    /// leaves it. Polling only runs while copy mode may be on, so an idle pane
    /// costs nothing.
    fn watch_copy_mode(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.tmux_session.clone() else {
            return;
        };
        if self.watching_copy_mode {
            return;
        }
        self.watching_copy_mode = true;
        cx.spawn(async move |pane, cx| {
            loop {
                cx.background_executor().timer(COPY_MODE_POLL).await;
                let s = session.clone();
                let in_mode = cx
                    .background_executor()
                    .spawn(async move { crate::tmux::pane_in_mode(&s) })
                    .await;
                let keep = pane
                    .update(cx, |pane, cx| {
                        if pane.copy_mode != in_mode {
                            pane.copy_mode = in_mode;
                            cx.notify();
                        }
                        in_mode
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
            let _ = pane.update(cx, |pane, _| pane.watching_copy_mode = false);
        })
        .detach();
    }

    /// A key typed in copy mode: leave copy mode, then deliver the key (and
    /// any typed meanwhile) to the agent, so typing after scrolling up just
    /// works instead of triggering copy-mode shortcuts like `t` (jump-to).
    fn type_through_copy_mode(&mut self, keystroke: Keystroke, cx: &mut Context<Self>) {
        self.held_keys.push(keystroke);
        if self.cancelling_copy_mode {
            return;
        }
        let Some(session) = self.tmux_session.clone() else {
            return;
        };
        self.cancelling_copy_mode = true;
        cx.spawn(async move |pane, cx| {
            cx.background_executor()
                .spawn(async move { crate::tmux::cancel_copy_mode(&session) })
                .await;
            let _ = pane.update(cx, |pane, cx| {
                pane.cancelling_copy_mode = false;
                pane.copy_mode = false;
                let keys = std::mem::take(&mut pane.held_keys);
                if let Some(view) = pane.view.clone() {
                    let terminal = view.read(cx).terminal().clone();
                    terminal.update(cx, |term, cx| {
                        let option_as_meta = TerminalSettings::get_global(cx).option_as_meta;
                        for key in &keys {
                            if !term.try_keystroke(key, option_as_meta)
                                && let Some(text) = &key.key_char
                            {
                                term.input(text.clone().into_bytes());
                            }
                        }
                    });
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Insert paths dropped from Finder into the running agent's prompt.
    fn drop_paths(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let Some(text) = paths_to_prompt_text(paths.paths()) else {
            return;
        };

        // `terminal()` returns a reference borrowed from `cx`, so it is cloned
        // out before `terminal.update(cx, …)` needs `cx` mutably.
        let handle = view.focus_handle(cx);
        let terminal = view.read(cx).terminal().clone();

        // Focus follows the paste — the point of the drop is to keep typing.
        window.focus(&handle, cx);

        // `paste`, not `input`: this wraps the text in bracketed-paste markers
        // when the running TUI has enabled that mode (Claude Code does), which
        // is what keeps a multi-file drop from being read as a submit.
        terminal.update(cx, |terminal, _| terminal.paste(&text));
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.view {
            Some(v) => v.read(cx).focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.respawn_pending {
            self.respawn_pending = false;
            // Respawn a plain shell, not the agent: quitting the agent is a
            // deliberate act, and relaunching it would trap the user in a loop.
            self.spawn(None, None, window, cx).detach();
        }
        let hx = theme(cx).clone();
        match &self.view {
            Some(view) => {
                let accent = hx.accent;
                div()
                    .size_full()
                    .bg(hx.bg0)
                    // Always laid out, painted the background colour, so
                    // `drag_over` below changes only the *colour*. Adding the
                    // border on hover instead would shrink the content box
                    // mid-drag, resize the pty, and make the agent redraw its
                    // whole prompt under the user's cursor.
                    //
                    // The border is the *entire* affordance, hence 2px: a
                    // background tint cannot work here. `TerminalView` wraps its
                    // content in an opaque `bg(editor_background)` fill, and
                    // `harness_zed_theme::hex` emits `#rrggbb` with no alpha, so
                    // anything painted behind the terminal is covered.
                    .border_2()
                    .border_color(hx.bg0)
                    .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(accent))
                    .on_drop(cx.listener(|pane, paths: &ExternalPaths, window, cx| {
                        pane.drop_paths(paths, window, cx);
                    }))
                    // Scrolling up or drag-selecting can put the tmux pane in
                    // copy mode; start following it.
                    .on_scroll(cx.listener(|pane, _: &ScrollEvent, _, cx| {
                        pane.watch_copy_mode(cx);
                    }))
                    .capture_any_mouse_down(cx.listener(|pane, _: &MouseDownEvent, _, cx| {
                        pane.watch_copy_mode(cx);
                    }))
                    .capture_key_down(cx.listener(|pane, ev: &KeyDownEvent, _, cx| {
                        if (pane.copy_mode || pane.cancelling_copy_mode)
                            && is_typing_key(&ev.keystroke)
                        {
                            cx.stop_propagation();
                            pane.type_through_copy_mode(ev.keystroke.clone(), cx);
                        }
                    }))
                    .relative()
                    .child(view.clone())
                    .when(self.copy_mode, |el| {
                        el.child(
                            div()
                                .absolute()
                                .bottom_2()
                                .right_3()
                                .px_2()
                                .py_0p5()
                                .rounded_md()
                                .bg(hx.bg2)
                                .border_1()
                                .border_color(hx.accent)
                                .text_size(px(11.0))
                                .text_color(hx.tx1)
                                .child("scrollback · type or Esc to return"),
                        )
                    })
            }
            None => div()
                .track_focus(&self.focus_handle)
                .size_full()
                .bg(hx.bg0)
                .p_2()
                .child(
                    div().text_size(px(12.0)).text_color(hx.tx2).child(
                        self.error
                            .clone()
                            .unwrap_or_else(|| format!("spawning {} …", self.spawned)),
                    ),
                ),
        }
    }
}

/// How often copy-mode state is polled while it may be on.
const COPY_MODE_POLL: std::time::Duration = std::time::Duration::from_millis(150);

/// Keys that mean "I'm typing to the agent": text, Enter, Backspace, Tab.
/// Navigation (arrows, page keys), Escape, and shortcuts with ⌘/⌃ keep their
/// copy-mode meaning, so scrollback stays scrollable from the keyboard.
fn is_typing_key(k: &Keystroke) -> bool {
    if k.modifiers.platform || k.modifiers.control || k.modifiers.function {
        return false;
    }
    matches!(k.key.as_str(), "enter" | "backspace" | "tab")
        || k.key_char
            .as_deref()
            .is_some_and(|c| !c.is_empty() && c.chars().all(|ch| !ch.is_control()))
}

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
        // filter_maps the same failure away silently, so a five-file drop
        // quietly becomes four with nothing said about it.
        //
        // In practice only `to_str` fires: `try_quote` refuses solely on an
        // interior nul, which an OS-supplied path cannot contain. It is matched
        // rather than unwrapped because that is shlex's contract, not ours.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ks(s: &'static str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn typing_keys_pass_through_copy_mode_navigation_does_not() {
        for k in [
            "t",
            "f",
            "shift-a",
            "space",
            "enter",
            "backspace",
            "tab",
            "1",
        ] {
            let mut key = ks(k);
            if key.key_char.is_none() && key.key.chars().count() == 1 {
                key.key_char = Some(key.key.clone());
            }
            if k == "space" {
                key.key_char = Some(" ".into());
            }
            if k == "shift-a" {
                key.key_char = Some("A".into());
            }
            assert!(is_typing_key(&key), "{k} should type");
        }
        for k in ["escape", "up", "pageup", "cmd-c", "ctrl-c", "left"] {
            assert!(!is_typing_key(&ks(k)), "{k} should stay in copy mode");
        }
    }

    #[test]
    fn single_clean_path_is_unquoted_and_padded() {
        assert_eq!(
            paths_to_prompt_text(&[PathBuf::from("/Users/example/notes.md")]),
            Some(" /Users/example/notes.md ".to_string())
        );
    }

    #[test]
    fn screenshot_name_with_spaces_is_quoted_intact() {
        // The case this feature exists for: macOS names every screenshot with
        // spaces, which is exactly what makes typing the path by hand awful.
        let p = PathBuf::from("/Users/example/Desktop/Screenshot 2026-01-01 at 9.00.00 AM.png");
        assert_eq!(
            paths_to_prompt_text(&[p]),
            Some(" '/Users/example/Desktop/Screenshot 2026-01-01 at 9.00.00 AM.png' ".to_string())
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
    fn shell_metacharacters_are_quoted_inert() {
        // Not cosmetic: this pane also runs a plain `Shell::System` (the
        // read-only `main` entry, and after an agent exits), so a path that
        // escaped quoting would land at a real shell prompt and eventually run.
        // Single-quoted shell strings do no escape processing at all, which is
        // what makes $(), backticks, ; and && inert here.
        let paths = [
            PathBuf::from("/tmp/$(whoami).png"),
            PathBuf::from("/tmp/a;rm -rf b.png"),
        ];
        assert_eq!(
            paths_to_prompt_text(&paths),
            Some(" '/tmp/$(whoami).png' '/tmp/a;rm -rf b.png' ".to_string())
        );
    }

    #[test]
    fn an_embedded_single_quote_switches_to_double_quotes() {
        // A single quote cannot appear inside a single-quoted shell string, so
        // shlex switches strategy rather than escaping. Pinned because it is the
        // least obvious branch of the quoting and the likeliest to regress
        // unnoticed if the implementation is ever swapped.
        assert_eq!(
            paths_to_prompt_text(&[PathBuf::from("/tmp/it's.png")]),
            Some(" \"/tmp/it's.png\" ".to_string())
        );
    }

    #[test]
    fn a_quote_does_not_open_a_double_quoted_window_for_metacharacters() {
        // The dangerous-looking combination, pinned deliberately: a double-quoted
        // shell string *does* expand $() and backticks, so if the quote above
        // forced the whole path into one pair of double quotes, a path could
        // execute. It does not — shlex quotes per *chunk*, double-quoting only
        // the run containing the apostrophe and single-quoting the run holding
        // the metacharacters, where nothing expands.
        assert_eq!(
            paths_to_prompt_text(&[PathBuf::from("/tmp/it's$(whoami)`id`.png")]),
            Some(" \"/tmp/it's\"'$(whoami)`id`.png' ".to_string())
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
            paths_to_prompt_text(&[bad, PathBuf::from("/tmp/ok.png")]),
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
