//! Terminal pane backed by zed's `terminal_view::TerminalView` (proper pty
//! sizing, keyboard input, IME, scrollback, and selection) instead of a custom
//! renderer.
//!
//! TerminalView needs a `WeakEntity<Workspace>` and `WeakEntity<Project>`, but
//! only uses them on graceful (`.upgrade().ok()`) paths — never on construction
//! or render — so we pass invalid handles and never create a real Workspace.
//!
//! On creation the pane runs **whichever agent the worktree records**
//! (§4.5) — pi, opencode, claude, codex, kimi — or a plain shell on the
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
    App, AppContext, Context, Entity, FocusHandle, Focusable, IntoElement, Render, Styled,
    WeakEntity, Window, div, prelude::*, px,
};
use project::Project;
use terminal::{
    Event as TerminalEvent, TerminalBuilder,
    terminal_settings::{AlternateScroll, CursorShape as SettingsCursorShape},
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
}

impl TerminalPane {
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
        };
        this.spawn(agent, session, window, cx);
        this
    }

    fn spawn(
        &mut self,
        agent: Option<AgentCommand>,
        session: Option<TerminalSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (shell, spawned, env) = match session {
            // tmux-backed: run a tmux client; the agent (and its env, inlined into
            // the script) live in the tmux server, so this pane's env is empty.
            Some(sess) => {
                let label = agent
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|| "shell".to_string());
                let args = crate::tmux::client_args(
                    &sess.name,
                    &self.worktree_root,
                    agent.as_ref().filter(|_| !sess.attach_only),
                    sess.attach_only,
                );
                (
                    Shell::WithArguments {
                        program: "tmux".to_string(),
                        args,
                        title_override: Some(label.clone()),
                    },
                    format!("{label} (tmux {})", sess.name),
                    Vec::new(),
                )
            }
            // No tmux: today's direct spawn.
            None => match agent {
                Some(cmd) => {
                    let spawned = format!("{} ({})", cmd.label, cmd.program.display());
                    (
                        Shell::WithArguments {
                            program: cmd.program.to_string_lossy().into_owned(),
                            args: cmd.args,
                            title_override: Some(cmd.label),
                        },
                        spawned,
                        cmd.env,
                    )
                }
                None => (Shell::System, "$SHELL".to_string(), Vec::new()),
            },
        };

        self.spawned = spawned;
        self.error = None;
        self.view = None;

        let builder_task = TerminalBuilder::new(
            Some(self.worktree_root.clone()),
            None,
            shell,
            // The adapter's env (e.g. HARNESS_AGENT_SCHEMA, so the pi tool
            // extension finds the contract from inside a packaged .app).
            env.into_iter().collect(),
            SettingsCursorShape::default(),
            AlternateScroll::On,
            None,
            Vec::new(),
            0,
            false,
            self.window_id,
            None,
            cx,
            Vec::new(),
            PathStyle::local(),
        );

        cx.spawn_in(window, async move |pane, cx| {
            let built = builder_task.await;
            let _ = pane.update_in(cx, |pane, window, cx| match built {
                Ok(builder) => {
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
                    window.focus(&handle, cx);
                    cx.notify();
                }
                Err(error) => {
                    pane.error = Some(format!("failed to spawn terminal: {error:#}"));
                    cx.notify();
                }
            });
        })
        .detach();
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
            self.spawn(None, None, window, cx);
        }
        let hx = theme(cx).clone();
        match &self.view {
            Some(view) => div().size_full().bg(hx.bg0).child(view.clone()),
            None => div()
                .size_full()
                .bg(hx.bg0)
                .p_2()
                .child(
                    div()
                        .text_size(px(12.0))
                        .text_color(hx.tx2)
                        .child(self.error.clone().unwrap_or_else(|| format!("spawning {} …", self.spawned))),
                ),
        }
    }
}
