//! Terminal pane backed by zed's `terminal_view::TerminalView` (proper pty
//! sizing, keyboard input, IME, scrollback, and selection) instead of a custom
//! renderer.
//!
//! TerminalView needs a `WeakEntity<Workspace>` and `WeakEntity<Project>`, but
//! only uses them on graceful (`.upgrade().ok()`) paths — never on construction
//! or render — so we pass invalid handles and never create a real Workspace.
//!
//! On creation the pane runs `pi` (worktree tabs) or a plain shell (`main`, D8)
//! with cwd = the configured root. When the process exits (e.g. you quit pi) the
//! pane respawns a plain shell in the same dir, so the tab stays usable.

use std::path::PathBuf;

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

pub struct TerminalPane {
    view: Option<Entity<TerminalView>>,
    /// Human-readable description of what's running (status bar chrome).
    pub spawned: String,
    error: Option<String>,
    worktree_root: PathBuf,
    window_id: u64,
    respawn_pending: bool,
    focus_handle: FocusHandle,
}

impl TerminalPane {
    pub fn new(
        worktree_root: PathBuf,
        spawn_agent: bool,
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
            focus_handle: cx.focus_handle(),
        };
        this.spawn(spawn_agent, window, cx);
        this
    }

    fn spawn(&mut self, use_agent: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (shell, spawned) = match use_agent.then(resolve_pi).flatten() {
            Some(pi) => (
                Shell::WithArguments {
                    program: pi.to_string_lossy().into_owned(),
                    args: Vec::new(),
                    title_override: Some("pi".to_string()),
                },
                format!("pi ({})", pi.display()),
            ),
            None if use_agent => (Shell::System, "$SHELL (pi not found on PATH)".to_string()),
            None => (Shell::System, "$SHELL".to_string()),
        };

        self.spawned = spawned;
        self.error = None;
        self.view = None;

        let builder_task = TerminalBuilder::new(
            Some(self.worktree_root.clone()),
            None,
            shell,
            Default::default(),
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
                        if matches!(event, TerminalEvent::CloseTerminal) {
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
            self.spawn(false, window, cx);
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

/// Find `pi` on $PATH (npm `@mariozechner/pi-coding-agent`).
fn resolve_pi() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("pi");
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}
