//! The PATH a Finder-launched bundle *should* have had.
//!
//! A `.app` started from Finder, Spotlight or `open` inherits launchd's
//! environment, not the user's shell: `PATH=/usr/bin:/bin:/usr/sbin:/sbin`.
//! Every tool this app depends on lives somewhere else — `/opt/homebrew/bin`
//! (tmux, gh), `~/.cargo/bin` (rust-analyzer), `~/.local/bin` (claude) — so the
//! installed build reported a machine with nothing installed while `cargo run`
//! from a terminal, which inherits the shell's PATH, found everything. `git` was
//! the single exception, and only because macOS ships `/usr/bin/git`.
//!
//! That misreading is not confined to the requirements screen: `resolve_on_path`
//! picks the agent CLI, `tmux` is spawned by bare name, and the worktree badges
//! shell out to `gh`. All of them read this one variable, so it is repaired once,
//! before anything else runs.
//!
//! The repair is to ask the user's own login shell what PATH it exports —
//! the same answer they would get in a terminal, including whatever their
//! `.zprofile`/`.zshrc` builds up — and merge it over what launchd gave us.

use std::ffi::{OsStr, OsString};
use std::time::{Duration, Instant};

/// Markers around the PATH in the probe's output, so a shell profile that
/// prints a banner (or a version-manager's chatter) cannot be mistaken for the
/// answer.
const BEGIN: &str = "__OPENORCHESTRA_PATH_BEGIN__";
const END: &str = "__OPENORCHESTRA_PATH_END__";

/// How long the login shell gets before we give up and keep launchd's PATH. A
/// profile that blocks forever must not take the app down with it.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The shell snippet that reports PATH between the markers.
fn probe_script() -> String {
    format!("printf '\\n%s%s%s\\n' '{BEGIN}' \"$PATH\" '{END}'")
}

/// The same, for Git Bash on Windows: its PATH is POSIX-spelled
/// (`/c/Users/me/.grok/bin:/usr/bin`); `cygpath -w -p` turns it into the
/// `C:\…;C:\…` list Windows APIs read.
fn windows_probe_script() -> String {
    format!("printf '\\n%s%s%s\\n' '{BEGIN}' \"$(cygpath -w -p \"$PATH\")\" '{END}'")
}

/// Pull the PATH back out of whatever the shell printed.
fn extract(raw: &str) -> Option<String> {
    let start = raw.find(BEGIN)? + BEGIN.len();
    let rest = &raw[start..];
    let end = rest.find(END)?;
    let path = rest[..end].trim();
    (!path.is_empty()).then(|| path.to_string())
}

/// Merge the login shell's PATH over the inherited one.
///
/// Shell entries come first: that is the order the user's own terminal resolves
/// in, so a Homebrew `git` shadowing `/usr/bin/git` in a terminal shadows it
/// here too. Inherited entries the shell did not mention are kept rather than
/// dropped — losing a directory would be a new failure of its own.
fn merge(inherited: Option<&OsStr>, from_shell: &str) -> Option<OsString> {
    fn push(dirs: &mut Vec<std::path::PathBuf>, dir: std::path::PathBuf) {
        if !dir.as_os_str().is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }

    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for dir in std::env::split_paths(from_shell) {
        push(&mut dirs, dir);
    }
    if dirs.is_empty() {
        return None;
    }
    for dir in inherited.into_iter().flat_map(std::env::split_paths) {
        push(&mut dirs, dir);
    }
    std::env::join_paths(dirs).ok()
}

/// Ask the user's login shell for its PATH.
///
/// `-l -i` because PATH is as often built in `.zshrc` (interactive) as in
/// `.zprofile` (login), and a probe that reads only one of them reports a PATH
/// the user does not recognise. stdout goes to a file rather than a pipe: a
/// chatty profile can outrun a 64 KiB pipe buffer we are not draining, and the
/// deadlock would be at startup, before any window exists to explain it.
fn ask_login_shell(script: &str) -> Option<String> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty())?;
    let out_path = std::env::temp_dir().join(format!("openorchestra-path-{}", std::process::id()));
    let out = std::fs::File::create(&out_path).ok()?;

    let mut child = std::process::Command::new(&shell)
        .args(["-l", "-i", "-c", script])
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + PROBE_TIMEOUT;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Err(_) => break false,
            Ok(None) if Instant::now() >= deadline => {
                log::warn!("{shell} did not report its PATH within {PROBE_TIMEOUT:?}");
                let _ = child.kill();
                break false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    };

    let raw = std::fs::read_to_string(&out_path).ok();
    let _ = std::fs::remove_file(&out_path);
    finished
        .then_some(raw)
        .flatten()
        .as_deref()
        .and_then(extract)
}

/// Whether this process needs the repair at all.
///
/// A terminal on stdout means we were started from a shell and already have its
/// environment; the probe would cost a shell spawn to learn nothing. Finder,
/// Spotlight and `open` all give a bundle no tty, which is exactly the case that
/// needs fixing.
fn launched_without_a_shell() -> bool {
    use std::io::IsTerminal as _;
    !std::io::stdout().is_terminal() && !std::io::stdin().is_terminal()
}

/// Repair `PATH` in place when this process was launched without a shell.
///
/// Call once, first thing in `main`, before any thread exists: setting an
/// environment variable races with any other thread reading one.
pub fn hydrate_path() {
    if std::env::var_os("HARNESS_NO_LOGIN_PATH").is_some() || !launched_without_a_shell() {
        return;
    }
    let Some(from_shell) = ask_login_shell(&probe_script()) else {
        return;
    };
    let Some(merged) = merge(std::env::var_os("PATH").as_deref(), &from_shell) else {
        return;
    };
    log::info!("PATH from login shell: {}", merged.to_string_lossy());
    // SAFETY: `main` calls this before spawning any thread, so no other thread
    // can be reading the environment concurrently.
    unsafe { std::env::set_var("PATH", merged) };
}

/// Windows: add the directories Git Bash's login shell puts on PATH that
/// this process lacks, and say whether any were added.
///
/// CLI installers run from Git Bash (Grok's, for one) put themselves on
/// PATH in `~/.bashrc` only, so the app — which resolves agents itself —
/// reported them missing while a terminal ran them fine. Unlike
/// [`hydrate_path`] this only appends: Windows' own PATH keeps precedence,
/// and Git's directories (`usr\bin` holds a `find`, `sort` and `link`) are
/// left to bash, which adds them itself.
///
/// Safe to call while threads run on Windows, where the environment block
/// is locked (`std::env` and the OS both).
pub fn add_login_shell_dirs() -> bool {
    let Some(from_shell) = ask_login_shell(&windows_probe_script()) else {
        return false;
    };
    let git_root = std::env::var_os("SHELL")
        .map(std::path::PathBuf::from)
        .and_then(|bash| Some(bash.parent()?.parent()?.to_path_buf()));
    let Some(added) = append_missing(
        std::env::var_os("PATH").as_deref(),
        &from_shell,
        git_root.as_deref(),
    ) else {
        return false;
    };
    log::info!("PATH: added from the login shell: {}", added.1.join(";"));
    // SAFETY: see above; this runs only on Windows.
    unsafe { std::env::set_var("PATH", added.0) };
    true
}

/// `inherited` plus the `from_shell` directories it lacks (compared without
/// case or trailing separator, as Windows does), skipping any under
/// `skip_root`. `None` when nothing is new; else the new PATH and what was
/// added.
fn append_missing(
    inherited: Option<&OsStr>,
    from_shell: &str,
    skip_root: Option<&std::path::Path>,
) -> Option<(OsString, Vec<String>)> {
    let norm = |p: &std::path::Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    let skip = skip_root.map(norm);
    let mut dirs: Vec<std::path::PathBuf> =
        inherited.into_iter().flat_map(std::env::split_paths).collect();
    let mut added = Vec::new();
    for dir in from_shell.split(';').map(str::trim).filter(|d| !d.is_empty()) {
        let dir = std::path::PathBuf::from(dir);
        let key = norm(&dir);
        let under_skip = skip
            .as_ref()
            .is_some_and(|root| key == *root || key.starts_with(&format!("{root}\\")));
        if !dir.is_absolute() || under_skip || dirs.iter().any(|d| norm(d) == key) {
            continue;
        }
        added.push(dir.to_string_lossy().into_owned());
        dirs.push(dir);
    }
    if added.is_empty() {
        return None;
    }
    Some((std::env::join_paths(dirs).ok()?, added))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_is_extracted_from_a_chatty_profile() {
        // The regression this whole module exists for is a *silently wrong*
        // answer, so a profile that prints a banner must not be able to produce
        // one: the markers are what make the answer identifiable.
        let raw =
            format!("nvm: using v20\nWelcome back!\n\n{BEGIN}/opt/homebrew/bin:/usr/bin{END}\n");
        assert_eq!(extract(&raw).as_deref(), Some("/opt/homebrew/bin:/usr/bin"));
    }

    #[test]
    fn output_without_markers_is_not_a_path() {
        assert_eq!(extract(""), None);
        assert_eq!(extract("/opt/homebrew/bin:/usr/bin"), None);
        assert_eq!(
            extract(&format!("{BEGIN}/opt/homebrew/bin")),
            None,
            "unterminated"
        );
        assert_eq!(extract(&format!("{BEGIN}   {END}")), None, "empty PATH");
    }

    #[test]
    fn the_script_prints_the_live_path_between_the_markers() {
        let script = probe_script();
        assert!(script.contains("\"$PATH\""), "must expand PATH: {script}");
        assert!(script.contains(BEGIN) && script.contains(END), "{script}");
        // Single-quoted markers: an expansion here would rewrite the fence we
        // then search for.
        assert!(script.contains(&format!("'{BEGIN}'")), "{script}");
    }

    #[test]
    fn shell_directories_come_first_and_inherited_ones_survive() {
        let merged = merge(
            Some(OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            "/opt/homebrew/bin:/Users/x/.cargo/bin:/usr/bin:/bin",
        )
        .unwrap();
        assert_eq!(
            merged.to_string_lossy(),
            // Homebrew ahead of /usr/bin, as in a terminal; /usr/sbin and /sbin
            // kept even though the shell never mentioned them; no duplicates.
            "/opt/homebrew/bin:/Users/x/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        );
    }

    #[test]
    fn an_empty_shell_answer_leaves_path_alone() {
        // Better launchd's PATH than no PATH: an empty merge result would make
        // every tool unfindable, including the ones we could already see.
        assert_eq!(merge(Some(OsStr::new("/usr/bin")), ""), None);
        assert_eq!(merge(None, ""), None);
    }

    #[test]
    fn a_missing_inherited_path_is_not_an_error() {
        let merged = merge(None, "/opt/homebrew/bin:/usr/bin").unwrap();
        assert_eq!(merged.to_string_lossy(), "/opt/homebrew/bin:/usr/bin");
    }

    // Windows path semantics (`;`, drive letters): only meaningful there.
    #[cfg(windows)]
    #[test]
    fn windows_appends_only_what_bash_adds_beyond_git_itself() {
        let (path, added) = append_missing(
            Some(OsStr::new(r"C:\Windows\system32;C:\Users\me\AppData\Roaming\npm")),
            r"C:\Users\me\.grok\bin;C:\Program Files\Git\usr\bin;c:\windows\System32\;C:\Program Files\Git\mingw64\bin;C:\Users\me\AppData\Roaming\npm",
            Some(std::path::Path::new(r"C:\Program Files\Git")),
        )
        .unwrap();
        assert_eq!(added, [r"C:\Users\me\.grok\bin"]);
        assert_eq!(
            path.to_string_lossy(),
            r"C:\Windows\system32;C:\Users\me\AppData\Roaming\npm;C:\Users\me\.grok\bin"
        );
        // Nothing new: leave PATH alone.
        assert!(append_missing(Some(OsStr::new(r"C:\a")), r"C:\A\", None).is_none());
    }

    #[test]
    fn the_windows_script_reports_a_windows_spelled_path() {
        let script = windows_probe_script();
        assert!(script.contains("cygpath -w -p"), "{script}");
        assert!(script.contains(&format!("'{BEGIN}'")), "{script}");
    }

    #[test]
    fn empty_entries_are_dropped_because_they_mean_the_current_directory() {
        // A trailing colon in someone's profile puts "." on PATH; inheriting it
        // would make tool discovery depend on the working directory.
        let merged = merge(Some(OsStr::new("/bin:")), "/opt/homebrew/bin::/usr/bin").unwrap();
        assert_eq!(merged.to_string_lossy(), "/opt/homebrew/bin:/usr/bin:/bin");
    }
}
