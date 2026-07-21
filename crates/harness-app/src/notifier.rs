//! Native OS notifications when an agent needs input (architecture §4.6, A5).
//!
//! Fed from the [`Bus`](agent_runtime::Bus) that every adapter publishes on, so
//! this is agent-neutral: it does not know or care whether the signal arrived
//! as a status file, an SSE event, or a hook shim.
//!
//! Three rules, each of which exists because breaking it makes the feature
//! worse than not having it:
//!
//! 1. **Only exact signals may claim "input needed"** (R11). Enforced upstream
//!    by `AgentSignal::should_notify`, so a best-effort adapter reporting
//!    turn-complete can never be dressed up as a request for input.
//! 2. **Suppress when the user is already looking at that worktree.** Telling
//!    someone about the prompt they are staring at is pure noise.
//! 3. **Only on a transition into Waiting**, never on a repeat, so a flapping
//!    adapter cannot spam the user.
//!
//! macOS delivery goes through `osascript`. That is deliberate: the
//! `UNUserNotificationCenter` route §4.6 names requires the process to be a
//! properly registered bundle, which is true for `openOrchestra.app` but *not*
//! for `cargo run` during development — so the native path would silently do
//! nothing exactly when a developer is trying to test it. `osascript` works in
//! both, with no new dependency. Linux uses `notify-send` for the same reason
//! (`notify-rust` remains the tidier long-term option).

use std::path::Path;
use std::process::{Command, Stdio};

/// Post a native "needs input" notification.
///
/// Fire-and-forget: a failure to notify must never disturb the app, so every
/// error path is swallowed after being logged.
pub fn notify_needs_input(worktree_name: &str, repo: &str, reason: &str) {
    let title = format!("{worktree_name} — {repo}");
    let body = reason.to_string();
    post(&title, &body);
}

#[cfg(target_os = "macos")]
fn post(title: &str, body: &str) {
    // AppleScript string literals take double quotes and backslash escapes;
    // anything else in a branch or repo name would break the script, so escape
    // rather than interpolate raw.
    let script = format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(title)
    );
    if let Err(e) = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        log::warn!("could not post notification: {e}");
    }
}

#[cfg(target_os = "linux")]
fn post(title: &str, body: &str) {
    // `--` so a title or body starting with a dash cannot parse as a flag,
    // matching the arg-injection discipline used for git and gh.
    if let Err(e) = Command::new("notify-send")
        .arg("--app-name=openOrchestra")
        .arg("--")
        .arg(title)
        .arg(body)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        log::warn!("could not post notification: {e}");
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn post(title: &str, body: &str) {
    log::info!("notification: {title} — {body}");
}

/// Escape a string for embedding in an AppleScript double-quoted literal.
#[cfg(target_os = "macos")]
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Human-readable label for a worktree, preferring its editable name.
pub fn label_for(name: &str, path: &Path) -> String {
    if name.is_empty() {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "macos")]
    #[test]
    fn applescript_literals_are_escaped() {
        // A branch called `feat/"quoted"` would otherwise terminate the
        // AppleScript string and change what gets executed.
        assert_eq!(super::escape(r#"a "b" c"#), r#"a \"b\" c"#);
        assert_eq!(super::escape(r"back\slash"), r"back\\slash");
    }

    #[test]
    fn label_falls_back_to_the_directory_name() {
        let path = std::path::Path::new("/wt/app/nav");
        assert_eq!(super::label_for("", path), "nav");
        assert_eq!(super::label_for("Navigation", path), "Navigation");
    }
}
