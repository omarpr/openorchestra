//! How to spawn a shell that behaves like a new terminal window.
//!
//! Opening Terminal.app gives you a shell that is both a **login** shell and an
//! **interactive** one, and that pair of adjectives is the whole story: zsh
//! reads `.zprofile` because it is a login shell and `.zshrc` *only* because it
//! is interactive. Everything this app used to spawn was non-interactive —
//! `sh -lc` for the agent inside tmux and for worktree hooks, `$SHELL -lc` for a
//! custom agent command — so `.zshrc` was never read, and the user's exports,
//! aliases and version-manager shims were missing from exactly the processes
//! they expected them in. The visible shell tabs looked fine, which is what made
//! it confusing: tmux runs its pane shell on a tty with no `-c`, so those *were*
//! interactive and did read `.zshrc`.
//!
//! Using `$SHELL` rather than `sh` matters for a second reason. `sh -lc` reads
//! `/etc/profile`, which on macOS runs `path_helper`; that rebuilds PATH with
//! the system directories first, pushing `/opt/homebrew/bin` and friends
//! *behind* `/usr/bin`. A pane could therefore resolve a different binary than
//! the user's own terminal does. A login+interactive `$SHELL` ends up with the
//! PATH the user actually sees, because their `.zprofile`/`.zshrc` run last.

/// The user's shell, or a POSIX fallback when `SHELL` says nothing useful.
fn shell_program() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_string())
}

/// Whether `program` is a bare POSIX shell that we should not ask for an
/// interactive session.
///
/// `-i` is what makes a shell read its rc file, but plain `sh`/`dash` have no
/// per-user rc worth reading and older `dash` rejects `-l` outright. Asking them
/// for a login+interactive session risks a shell that fails to start at all,
/// which is worse than one missing an rc file it never had.
fn is_bare_posix_sh(program: &str) -> bool {
    let base = program.rsplit('/').next().unwrap_or(program);
    matches!(base, "sh" | "dash")
}

/// The `(program, args)` that runs `script` the way a new terminal window would.
///
/// `-l -i` (as one `-lic`, the form these shells accept): login so `.zprofile`
/// runs, interactive so `.zshrc` does. Callers pass a shell *line*, not an argv,
/// so anything the user could type — an alias, a shell function, a
/// version-manager shim — resolves the same way it would in their terminal.
pub fn interactive_login_command(script: &str) -> (String, Vec<String>) {
    let program = shell_program();
    let flags = if is_bare_posix_sh(&program) { "-lc" } else { "-lic" };
    (program, vec![flags.to_string(), script.to_string()])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialized because they all move the process-wide `SHELL`.
    fn with_shell<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("SHELL");
        // SAFETY: the lock serializes these tests against each other, and
        // nothing else in this crate reads SHELL.
        unsafe {
            match value {
                Some(v) => std::env::set_var("SHELL", v),
                None => std::env::remove_var("SHELL"),
            }
        }
        let out = f();
        unsafe {
            match previous {
                Some(v) => std::env::set_var("SHELL", v),
                None => std::env::remove_var("SHELL"),
            }
        }
        out
    }

    #[test]
    fn zsh_is_asked_for_a_login_and_interactive_session() {
        // The bug this module exists for: without `-i`, zsh skips `.zshrc`, so
        // the user's exports and aliases are missing from the spawned process.
        let (program, args) = with_shell(Some("/bin/zsh"), || {
            interactive_login_command("claude --resume")
        });
        assert_eq!(program, "/bin/zsh");
        assert_eq!(args, vec!["-lic", "claude --resume"]);
    }

    #[test]
    fn the_users_own_shell_is_used_not_sh() {
        // `sh -lc` reads /etc/profile, whose path_helper reorders PATH so
        // Homebrew lands behind /usr/bin — a pane could resolve a different
        // binary than the user's terminal does.
        let (program, args) = with_shell(Some("/opt/homebrew/bin/fish"), || {
            interactive_login_command("true")
        });
        assert_eq!(program, "/opt/homebrew/bin/fish");
        assert_eq!(args[0], "-lic");
    }

    #[test]
    fn bare_posix_shells_are_not_asked_for_an_interactive_session() {
        // Nothing to read, and older dash rejects -l: a shell that fails to
        // start is worse than one missing an rc file it never had.
        for sh in ["/bin/sh", "sh", "/bin/dash"] {
            let (program, args) = with_shell(Some(sh), || interactive_login_command("true"));
            assert_eq!(program, sh);
            assert_eq!(args[0], "-lc", "{sh} should stay non-interactive");
        }
    }

    #[test]
    fn an_unset_or_blank_shell_falls_back_to_sh() {
        for value in [None, Some(""), Some("   ")] {
            let (program, args) = with_shell(value, || interactive_login_command("true"));
            assert_eq!(program, "/bin/sh", "SHELL={value:?}");
            assert_eq!(args[0], "-lc");
        }
    }

    #[test]
    fn the_script_is_passed_through_verbatim() {
        // The caller has already quoted it; a second round of escaping here
        // would corrupt paths with spaces.
        let script = "HARNESS_X='/tmp/a b' '/usr/bin/pi' '--mode' 'tui'";
        let (_, args) = with_shell(Some("/bin/zsh"), || interactive_login_command(script));
        assert_eq!(args[1], script);
    }
}
