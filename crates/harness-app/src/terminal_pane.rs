//! Spike-quality terminal pane.
//!
//! Model layer: zed's `terminal` crate (pty management + alacritty state
//! machine + gpui event plumbing), consumed out-of-tree from the pinned rev.
//! View layer: a minimal custom renderer (grid of styled text runs + block
//! cursor) because zed's `terminal_view` crate drags the `workspace` dependency
//! cone, which is out of scope for Spike 1 (it arrives with Spike 3's editor).
//!
//! On creation the pane spawns `pi` (resolved from $PATH) with cwd = the
//! configured worktree root, falling back to the system shell when pi is not
//! installed. See docs/architecture.md §4.1.

use std::path::PathBuf;

use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, Pixels,
    Render, SharedString, Styled, StyledText, TextRun, Window, canvas, div, font, prelude::*, px,
};
use terminal::{
    Color, CursorShape, Event as TerminalEvent, NamedColor, Terminal, TerminalBounds,
    TerminalBuilder,
    terminal_settings::{AlternateScroll, CursorShape as SettingsCursorShape},
};
use theme::ActiveTheme;
use util::paths::PathStyle;
use util::shell::Shell;

use crate::theme::{HarnessTheme, theme};

const FONT_SIZE: f32 = 13.0;
const LINE_HEIGHT: f32 = 19.0;

pub struct TerminalPane {
    focus_handle: FocusHandle,
    terminal: Option<gpui::Entity<Terminal>>,
    /// Human-readable description of what was spawned (for the status bar /
    /// placeholder chrome).
    pub spawned: String,
    error: Option<String>,
    exited: bool,
    pane_bounds: Option<Bounds<Pixels>>,
    applied_size: Option<(Pixels, Pixels, Bounds<Pixels>)>,
}

impl TerminalPane {
    pub fn new(
        worktree_root: PathBuf,
        spawn_agent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // The `main` entry is a read-only workspace with no agent (§6.1 D8), so
        // it gets a plain shell; worktree tabs spawn pi.
        let (shell, spawned) = match spawn_agent.then(resolve_pi).flatten() {
            Some(pi) => (
                Shell::WithArguments {
                    program: pi.to_string_lossy().into_owned(),
                    args: Vec::new(),
                    title_override: Some("pi".to_string()),
                },
                format!("pi ({})", pi.display()),
            ),
            None if spawn_agent => (Shell::System, "$SHELL (pi not found on PATH)".to_string()),
            None => (Shell::System, "$SHELL (main — no agent)".to_string()),
        };

        let window_id = window.window_handle().window_id().as_u64();
        let builder_task = TerminalBuilder::new(
            Some(worktree_root),
            None,                          // task
            shell,
            Default::default(),            // extra env
            SettingsCursorShape::default(),
            AlternateScroll::On,
            None,                          // max_scroll_history_lines -> default
            Vec::new(),                    // path hyperlink regexes
            0,                             // path hyperlink timeout
            false,                         // is_remote_terminal
            window_id,
            None,                          // completion_tx
            cx,
            Vec::new(),                    // activation script
            PathStyle::local(),
        );

        cx.spawn(async move |pane, cx| {
            let built = builder_task.await;
            pane.update(cx, |pane, cx| match built {
                Ok(builder) => {
                    let terminal = cx.new(|cx| builder.subscribe(cx));
                    cx.subscribe(&terminal, |pane, _, event: &TerminalEvent, cx| {
                        match event {
                            TerminalEvent::Wakeup => cx.notify(),
                            TerminalEvent::CloseTerminal => {
                                pane.exited = true;
                                cx.notify();
                            }
                            _ => {}
                        }
                    })
                    .detach();
                    pane.terminal = Some(terminal);
                    cx.notify();
                }
                Err(error) => {
                    pane.error = Some(format!("failed to spawn terminal: {error:#}"));
                    cx.notify();
                }
            })
        })
        .detach();

        Self {
            focus_handle: cx.focus_handle(),
            terminal: None,
            spawned,
            error: None,
            exited: false,
            pane_bounds: None,
            applied_size: None,
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(terminal) = self.terminal.clone() else {
            return;
        };
        let keystroke = event.keystroke.clone();
        let handled = terminal.update(cx, |term, _| {
            if term.try_keystroke(&keystroke, false) {
                true
            } else if let Some(text) = keystroke.key_char.clone() {
                if keystroke.modifiers.control || keystroke.modifiers.platform {
                    false
                } else {
                    term.input(text.into_bytes());
                    true
                }
            } else {
                false
            }
        });
        if handled {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Focusable for TerminalPane {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx).clone();
        let mono = font(hx.font_mono);
        let font_size = px(FONT_SIZE);
        let line_height = px(LINE_HEIGHT);
        let font_id = cx.text_system().resolve_font(&mono);
        let cell_width = cx
            .text_system()
            .advance(font_id, font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(px(8.0));

        // Apply pane bounds (recorded by the canvas overlay on the previous
        // frame) to the pty/grid size.
        if let (Some(bounds), Some(terminal)) = (self.pane_bounds, self.terminal.clone()) {
            let key = (line_height, cell_width, bounds);
            if self.applied_size != Some(key) {
                self.applied_size = Some(key);
                terminal.update(cx, |term, _| {
                    term.set_size(TerminalBounds::new(line_height, cell_width, bounds));
                });
            }
        }

        // Drain pty events and snapshot the renderable grid.
        let content = self.terminal.clone().map(|terminal| {
            terminal.update(cx, |term, cx| {
                term.sync(window, cx);
                term.last_content().clone()
            })
        });

        let pane = cx.entity();
        let size_probe = canvas(
            move |bounds, _window, cx| {
                pane.update(cx, |pane, cx| {
                    if pane.pane_bounds != Some(bounds) {
                        pane.pane_bounds = Some(bounds);
                        cx.notify();
                    }
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let mut grid = div()
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .font_family(hx.font_mono)
            .text_size(font_size)
            .line_height(line_height)
            .text_color(hx.tx0);

        if let Some(content) = &content {
            for (text, runs) in build_lines(content, &mono, &hx, cx) {
                grid = grid.child(
                    div()
                        .h(line_height)
                        .flex_none()
                        .child(StyledText::new(SharedString::from(text)).with_runs(runs)),
                );
            }
        }

        let mut root = div()
            .id("terminal-pane")
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _, window, cx| {
                    window.focus(&pane.focus_handle, cx);
                }),
            )
            .on_key_down(cx.listener(Self::handle_key_down))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(hx.bg0)
            .p_2()
            .child(size_probe)
            .child(grid);

        // Block cursor overlay.
        if let Some(content) = &content {
            let row = content.cursor.point.line + content.display_offset as i32;
            if content.cursor.shape != CursorShape::Hidden && row >= 0 {
                root = root.child(
                    div()
                        .absolute()
                        .left(px(8.0) + cell_width * content.cursor.point.column as f32)
                        .top(px(8.0) + line_height * row as f32)
                        .w(cell_width)
                        .h(line_height)
                        .bg(crate::theme::tint(hx.accent, 0.55)),
                );
            }
        }

        if content.is_none() || self.error.is_some() || self.exited {
            let message = if let Some(error) = &self.error {
                error.clone()
            } else if self.exited {
                "process exited — restart the app to respawn".to_string()
            } else {
                format!("spawning {} …", self.spawned)
            };
            root = root.child(
                div()
                    .absolute()
                    .bottom_2()
                    .left_2()
                    .text_size(px(12.0))
                    .text_color(hx.tx2)
                    .child(message),
            );
        }

        root
    }
}

/// Group the viewport cells into per-row (text, styled runs) pairs.
fn build_lines(
    content: &terminal::Content,
    mono: &gpui::Font,
    hx: &HarnessTheme,
    cx: &App,
) -> Vec<(String, Vec<TextRun>)> {
    let mut lines: Vec<(String, Vec<TextRun>)> = Vec::new();
    let mut current_line: Option<i32> = None;

    for indexed in &content.cells {
        if current_line != Some(indexed.point.line) {
            current_line = Some(indexed.point.line);
            lines.push((String::new(), Vec::new()));
        }
        if indexed.cell.is_wide_char_spacer() {
            continue;
        }
        let (text, runs) = lines.last_mut().expect("pushed above");

        let mut fg = indexed.cell.foreground();
        let mut bg = indexed.cell.background();
        if indexed.cell.is_inverse() {
            std::mem::swap(&mut fg, &mut bg);
        }
        let color = convert_color(&fg, hx, cx);

        let ch = indexed.cell.character();
        let len = ch.len_utf8();
        text.push(ch);
        match runs.last_mut() {
            Some(run) if run.color == color => run.len += len,
            _ => runs.push(TextRun {
                len,
                font: mono.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    lines
}

/// Map a vte color to Hsla. Named/indexed colors go through the zed theme
/// (loaded via `theme_settings::init(LoadThemes::JustBase)`); defaults are
/// overridden to the harness tokens so the pane matches the chrome.
fn convert_color(color: &Color, hx: &HarnessTheme, cx: &App) -> gpui::Hsla {
    match color {
        Color::Named(NamedColor::Foreground) => gpui::Rgba::from(hx.tx0).into(),
        Color::Named(NamedColor::Background) => gpui::Rgba::from(hx.bg0).into(),
        Color::Named(named) => {
            let colors = cx.theme().colors();
            match named {
                NamedColor::Black => colors.terminal_ansi_black,
                NamedColor::Red => colors.terminal_ansi_red,
                NamedColor::Green => colors.terminal_ansi_green,
                NamedColor::Yellow => colors.terminal_ansi_yellow,
                NamedColor::Blue => colors.terminal_ansi_blue,
                NamedColor::Magenta => colors.terminal_ansi_magenta,
                NamedColor::Cyan => colors.terminal_ansi_cyan,
                NamedColor::White => colors.terminal_ansi_white,
                NamedColor::BrightBlack => colors.terminal_ansi_bright_black,
                NamedColor::BrightRed => colors.terminal_ansi_bright_red,
                NamedColor::BrightGreen => colors.terminal_ansi_bright_green,
                NamedColor::BrightYellow => colors.terminal_ansi_bright_yellow,
                NamedColor::BrightBlue => colors.terminal_ansi_bright_blue,
                NamedColor::BrightMagenta => colors.terminal_ansi_bright_magenta,
                NamedColor::BrightCyan => colors.terminal_ansi_bright_cyan,
                NamedColor::BrightWhite => colors.terminal_ansi_bright_white,
                NamedColor::DimBlack => colors.terminal_ansi_dim_black,
                NamedColor::DimRed => colors.terminal_ansi_dim_red,
                NamedColor::DimGreen => colors.terminal_ansi_dim_green,
                NamedColor::DimYellow => colors.terminal_ansi_dim_yellow,
                NamedColor::DimBlue => colors.terminal_ansi_dim_blue,
                NamedColor::DimMagenta => colors.terminal_ansi_dim_magenta,
                NamedColor::DimCyan => colors.terminal_ansi_dim_cyan,
                NamedColor::DimWhite => colors.terminal_ansi_dim_white,
                NamedColor::BrightForeground => colors.terminal_bright_foreground,
                NamedColor::DimForeground => colors.terminal_dim_foreground,
                NamedColor::Cursor => colors.terminal_foreground,
                NamedColor::Foreground | NamedColor::Background => unreachable!(),
            }
        }
        Color::Spec(rgb) => terminal::rgba_color(rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => terminal::get_color_at_index(*i as usize, cx.theme()),
    }
}

/// Find `pi` on $PATH (the Node.js CLI installed by
/// `npm install -g @mariozechner/pi-coding-agent`).
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
