//! The OpenCode adapter (§4.5, roadmap A2).
//!
//! **OpenCode 2 reports from inside the session.** OpenCode 2 (verified on
//! v2.3.2) runs one shared background service per Mac on a random port
//! (`opencode service status`) and streams every session on the Mac from it.
//! Nothing listens on the old fixed port 4096, so the SSE pump this adapter
//! used to run never connected. Instead the launcher
//! (`opencode-remote-launch.mjs`) appends a TUI plugin,
//! `shims/opencode-status/`, to OpenCode's inline CLI config
//! (`OPENCODE_CLI_CONFIG_CONTENT`, key `plugins`). The plugin runs in the
//! tab's own TUI process, with the tab's env, and writes the shared status
//! file through `remote-tab.mjs` — `.harness/status.json` plus
//! `status-tab-<HARNESS_AGENT_TAB_ID>.json`, `"agent": "opencode"` — exactly
//! like the Claude Code / Codex / Grok hooks. So [`SignalKind::StatusFile`]:
//! the app's `StatusFileSource` picks it up with per-tab attribution.
//!
//! The OpenCode 2 TUI plugin API this relies on, read from the v2.3.2 binary
//! (its bundled `@opencode/plugin/tui` and built-in `opencode.notifications`
//! plugin) since the 2.x API ships no typings on disk:
//!
//! - `Plugin.define({ id, setup(context) })` — `define` is the identity; a
//!   function returned from `setup` runs on unload.
//! - `context.ui.router.current()` → `{ type: "session", sessionID }` (or
//!   `home` / `plugin`).
//! - `context.data.listen(cb)` — called with `{ name, details }` for every
//!   event, after the TUI's own store has applied it.
//! - `context.data.session.status(id)` → `"running"` / `"idle"`;
//!   `.permission.list(id)` and `.form.list(id)` → pending requests;
//!   `.root(id)` / `.family(root)` → the subagent tree.
//!
//! Event → state mapping (§4.5). OpenCode 2 renamed the 1.x events; the
//! plugin reads the store those events maintain, which gives this table:
//!
//! | OpenCode 2 event (1.x name) | → |
//! |---|---|
//! | `permission.asked` (`permission.updated`), `form.created`, during a turn | `Waiting` / `PermissionPrompt` |
//! | `permission.replied`, `form.replied` / `form.cancelled` | back to `Running` (or `Idle`) |
//! | `session.execution.started` | `Running` |
//! | `session.execution.succeeded` / `failed` / `interrupted` (`session.idle` / `session.error`) | `Idle` / `TurnComplete` |
//!
//! A request still listed when its session stops running is ignored from
//! then on: OpenCode drops it on interrupt without a `replied` event, and
//! the store would otherwise show it as waiting through the next turn.
//!
//! Streaming progress (`message.*`, `session.updated`) is not mapped: it also
//! arrives after a turn ends and would pin the tab on running.
//!
//! The plugin reports only its own sessions — those its TUI has shown since
//! launch, plus their subagents — never the rest of the shared service.
//!
//! Fidelity is **Exact**: a pending permission is a real, out-of-band "I am
//! blocked on you", so this adapter may raise an OS notification (R11).
//!
//! **OpenCode 1.x** cannot load that plugin: its TUI plugin API is a
//! different shape (`(api, options, meta) => Promise<void>` in
//! `@opencode-ai/plugin` 1.x), and `@opencode/plugin/tui` does not exist
//! there. For 1.x only, [`AgentRuntime::subscribe`] keeps the old fallback:
//! the per-session server's SSE `/event` on port 4096, mapped by
//! [`classify`]. That stream carries no tab id. The installed CLI's
//! `--version` decides, off the UI thread and once per binary; OpenCode 2
//! gets no stream.
//!
//! The launcher injects the plugin into every app tab without checking the
//! version, so 1.x tabs get it too. How 1.x treats that entry (ignored, or
//! a plugin-load error) is unverified: no 1.x was available to try.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::sse::SseStream;
use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, AgentSignal, AgentState, SignalKind,
    WaitFidelity, WaitReason,
};

/// Port OpenCode's server listens on by default.
pub const DEFAULT_PORT: u16 = 4096;

/// The default launch command — THE one definition for this agent. The spawn
/// below and the settings UI (via [`crate::default_command_display`]) both
/// derive from these two consts.
pub(crate) const PROGRAM: &str = "opencode";
pub(crate) const DEFAULT_ARGS: &[&str] = &[];
const LAUNCHER: &str = include_str!("../shims/opencode-remote-launch.mjs");
const TUI_PLUGIN: &str = include_str!("../shims/opencode-remote-tui.js");
const TUI_PACKAGE: &str = r#"{"name":"openorchestra-remote","private":true,"type":"module","exports":{"./tui":"./tui.js"}}"#;

const STATUS_PLUGIN: &str = include_str!("../shims/opencode-status-tui.js");
const STATUS_CORE: &str = include_str!("../shims/opencode-status.mjs");
const STATUS_PACKAGE: &str = r#"{"name":"openorchestra-status","private":true,"type":"module","exports":{"./tui":"./tui.js"}}"#;

pub struct OpenCodeRuntime {
    worktree_root: PathBuf,
    host: String,
    port: u16,
    state_dir: Option<PathBuf>,
    /// Stream SSE only when the installed CLI is 1.x. Off for an explicit
    /// endpoint, whose caller already knows a server is there.
    legacy_only: bool,
    /// Set on drop so the subscription thread stops reconnecting.
    stopped: Arc<AtomicBool>,
}

impl OpenCodeRuntime {
    pub fn new(worktree_root: impl Into<PathBuf>) -> Self {
        let mut runtime = OpenCodeRuntime::with_endpoint(worktree_root, "127.0.0.1", DEFAULT_PORT);
        runtime.legacy_only = true;
        runtime
    }

    /// Stream SSE from a specific endpoint, whatever the installed CLI
    /// (tests, or a user-configured OpenCode 1.x port).
    pub fn with_endpoint(worktree_root: impl Into<PathBuf>, host: &str, port: u16) -> Self {
        OpenCodeRuntime {
            worktree_root: worktree_root.into(),
            host: host.to_string(),
            port,
            state_dir: None,
            legacy_only: false,
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn with_state_dir(mut self, state_dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(state_dir.into());
        self
    }
}

impl Drop for OpenCodeRuntime {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

impl AgentRuntime for OpenCodeRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }

    fn fidelity(&self) -> WaitFidelity {
        // A pending permission is a real blocked-on-you state.
        WaitFidelity::Exact
    }

    fn signal_kind(&self) -> SignalKind {
        // The status plugin writes the shared status file. (OpenCode 1.x
        // falls back to `subscribe`'s SSE stream.)
        SignalKind::StatusFile
    }

    fn ignored_paths(&self) -> &'static [&'static str] {
        &[".harness/"]
    }

    fn on_spawn(&self, _worktree_root: &Path) -> std::io::Result<()> {
        if let Some(dir) = &self.state_dir {
            let shims = dir.join("shims");
            let plugin = shims.join("opencode-remote");
            let status = shims.join("opencode-status");
            fs::create_dir_all(&plugin)?;
            fs::create_dir_all(&status)?;
            // The status plugin writes through the hooks' shared helper.
            crate::install_remote_tab_helper(dir)?;
            for (dir, name, content) in [
                (&plugin, "package.json", TUI_PACKAGE),
                (&plugin, "tui.js", TUI_PLUGIN),
                (&status, "package.json", STATUS_PACKAGE),
                (&status, "tui.js", STATUS_PLUGIN),
                (&status, "status.mjs", STATUS_CORE),
                (&shims, "opencode-remote-launch.mjs", LAUNCHER),
            ] {
                let file = dir.join(name);
                if fs::read_to_string(&file).ok().as_deref() != Some(content) {
                    // Other launches can install the same shim concurrently.
                    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
                    tmp.write_all(content.as_bytes())?;
                    crate::persist_shim(tmp, &file, content)?;
                }
            }
        }
        Ok(())
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        Some(AgentCommand {
            program: resolve_on_path(PROGRAM)?,
            args: DEFAULT_ARGS.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            label: PROGRAM.to_string(),
        })
    }

    fn prepare_launch(&self, mut command: AgentCommand) -> AgentCommand {
        let Some(dir) = &self.state_dir else {
            return command;
        };
        let launcher = dir.join("shims/opencode-remote-launch.mjs");
        if let (Some(node), true, Some(launcher), Some(program)) = (
            resolve_on_path("node"),
            launcher.is_file(),
            launcher.to_str(),
            command.program.to_str(),
        ) {
            command
                .args
                .splice(..0, [launcher.to_owned(), program.to_owned()]);
            command.program = node;
        }
        command
    }

    /// OpenCode 1.x only: its per-session server's SSE stream. OpenCode 2
    /// reports through the status file, so this thread ends at once there.
    fn subscribe(&self) -> Option<Receiver<AgentSignal>> {
        let probe = match self.legacy_only {
            true => Some(resolve_on_path(PROGRAM)?),
            false => None,
        };
        let (tx, rx) = unbounded();
        let host = self.host.clone();
        let port = self.port;
        let root = self.worktree_root.clone();
        let stopped = Arc::clone(&self.stopped);

        std::thread::Builder::new()
            .name("opencode-events".into())
            .spawn(move || {
                if probe.is_some_and(|program| !is_legacy_cli(&program)) {
                    return; // OpenCode 2: dropping `tx` ends the stream.
                }
                pump(&host, port, &root, &tx, &stopped)
            })
            .ok()?;
        Some(rx)
    }
}

/// Whether `program` is OpenCode 1.x, probed once per binary: the answer is
/// cached for the process, keyed by path and modification time so an
/// upgrade in place is probed again.
fn is_legacy_cli(program: &Path) -> bool {
    type Probes = Mutex<HashMap<(PathBuf, Option<SystemTime>), bool>>;
    static PROBES: OnceLock<Probes> = OnceLock::new();
    let modified = fs::metadata(program).and_then(|m| m.modified()).ok();
    let key = (program.to_path_buf(), modified);
    let probes = PROBES.get_or_init(Default::default);
    if let Some(&legacy) = probes.lock().unwrap().get(&key) {
        return legacy;
    }
    // Probed outside the lock: concurrent launches may both run it once.
    let legacy = legacy_server(&cli_version(program));
    probes.lock().unwrap().insert(key, legacy);
    legacy
}

/// `opencode --version`, or empty when it cannot be run.
fn cli_version(program: &Path) -> String {
    std::process::Command::new(program)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

/// Whether `--version` output names OpenCode 1.x or older, the generation
/// that served `/event` per session. Unknown output is not legacy: a stream
/// that never connects is worth nothing.
fn legacy_server(version: &str) -> bool {
    let major = version.split_whitespace().find_map(|word| {
        let word = word.trim_start_matches('v');
        let (major, _) = word.split_once('.')?;
        major.parse::<u64>().ok()
    });
    major.is_some_and(|major| major < 2)
}

/// Connect, translate events, and reconnect while anyone is listening.
///
/// The server is not up until the user actually starts `opencode`, and it goes
/// away when they quit, so a failed connect is the normal case rather than an
/// error — retry quietly and let the terminal keep working regardless.
fn pump(
    host: &str,
    port: u16,
    worktree_root: &Path,
    tx: &Sender<AgentSignal>,
    stopped: &AtomicBool,
) {
    while !stopped.load(Ordering::Relaxed) {
        match SseStream::connect(host, port, "/event") {
            Ok(stream) => {
                for event in stream {
                    if stopped.load(Ordering::Relaxed) {
                        return;
                    }
                    let Some((state, reason)) = classify(&event.event, &event.data) else {
                        continue;
                    };
                    let signal = AgentSignal {
                        worktree_root: worktree_root.to_path_buf(),
                        tab_id: None,
                        kind: AgentKind::OpenCode,
                        state,
                        reason,
                        // OpenCode keeps its own session store; nothing
                        // harvestable is exposed on this stream (§6.4).
                        session_file: None,
                        tool: None,
                    };
                    if tx.send(signal).is_err() {
                        return; // nobody listening any more
                    }
                }
            }
            Err(_) => {
                // Server not up yet (or just went away).
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        // A dropped receiver is detected by the `tx.send` failure inside the
        // loop above, which returns outright; reaching here just means the
        // server went away, so pause before reconnecting.
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}

/// Map an OpenCode 1.x event to a normalized state (the SSE fallback; the
/// OpenCode 2 plugin's `opencode-status.mjs` keeps the same semantics).
///
/// The event *type* may arrive either as the SSE `event:` field or as a `type`
/// property inside the JSON payload, depending on how the server frames it, so
/// both are checked. Returns `None` for anything unrecognized rather than
/// guessing — a false `Waiting` would raise a bogus "needs input" notification,
/// which is the one failure mode R11 exists to prevent.
pub fn classify(event_name: &Option<String>, data: &str) -> Option<(AgentState, WaitReason)> {
    let from_payload = serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_string));
    let name = event_name.clone().or(from_payload)?;

    match name.as_str() {
        "permission.updated" => Some((AgentState::Waiting, WaitReason::PermissionPrompt)),
        // A replied permission means the block cleared and work resumed.
        "permission.replied" => Some((AgentState::Running, WaitReason::None)),
        "session.idle" => Some((AgentState::Idle, WaitReason::TurnComplete)),
        "session.error" => Some((AgentState::Idle, WaitReason::TurnComplete)),
        // Any streaming progress means the agent is working.
        "message.updated" | "message.part.updated" | "session.updated" => {
            Some((AgentState::Running, WaitReason::None))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{is_legacy_cli, legacy_server};

    #[cfg(unix)]
    #[test]
    fn the_version_probe_runs_once_per_binary() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("opencode");
        let runs = dir.path().join("runs");
        std::fs::write(
            &program,
            format!("#!/bin/sh\necho x >> '{}'\necho 1.0.150\n", runs.display()),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(is_legacy_cli(&program));
        assert!(is_legacy_cli(&program));
        assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 1);
    }

    #[test]
    fn only_opencode_1_gets_the_sse_fallback() {
        assert!(!legacy_server("opencode v2.3.2\n"));
        assert!(!legacy_server("2.0.0"));
        assert!(legacy_server("1.0.150\n"));
        assert!(legacy_server("opencode v0.15.8"));
        assert!(!legacy_server(""));
        assert!(!legacy_server("opencode dev"));
    }
}
