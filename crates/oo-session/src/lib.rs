//! `oo-session`: what tmux gives openOrchestra on macOS, for Windows.
//!
//! Windows has no tmux, so an agent tab used to die with its tab and nothing
//! reattached after a restart. This crate is a small stand-in: each session is
//! a detached **host** process that owns one ConPTY and the program running in
//! it, keeps a terminal model of its screen (`vt100`), and serves attach
//! clients over a per-user named pipe. The **client** runs as the terminal
//! pane's process, exactly as `tmux attach` does on macOS: it relays the
//! pane's keystrokes to the host and the host's output to the pane, forwards
//! resizes, and on attach first redraws the session's screen and scrollback.
//!
//! The command line is the subset of tmux's that the app speaks
//! (`crates/harness-app/src/tmux.rs`): `new-session`, `attach-session`,
//! `list-sessions -F`, `list-panes -a -F`, `has-session`, `kill-session`,
//! `send-keys`, `set-option`, `display-message -p`, `if-shell -F`, joined by
//! bare `;` arguments, plus the part of control mode (`-C attach-session`)
//! that the phone remote's bridge uses. So the app's tmux code runs unchanged
//! against it; only the program name differs.
//!
//! Sessions are one process each (not one server for all, as tmux does), so a
//! crash takes down one session, and the list of sessions is simply the list
//! of this user's pipes.

pub mod cmdline;
pub mod format;
pub mod keys;
pub mod quote;

#[cfg(windows)]
mod win;

/// The program's version, as `-V` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the command line `args` (without the program name); returns the exit
/// code.
pub fn run(args: &[String]) -> i32 {
    #[cfg(windows)]
    {
        win::cli::run(args)
    }
    #[cfg(not(windows))]
    {
        let _ = args;
        eprintln!("oo-session is the Windows stand-in for tmux; use tmux on this platform");
        1
    }
}
