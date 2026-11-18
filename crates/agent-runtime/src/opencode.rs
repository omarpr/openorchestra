//! The OpenCode adapter (§4.5, roadmap A2) — the reference *peer*.
//!
//! OpenCode is the cleanest non-pi signal surface: its CLI runs a local HTTP
//! server alongside the TUI, and `GET /event` is a server-sent-event stream of
//! everything happening in the session. So unlike pi (a file) or Claude Code (a
//! hook shim), this adapter translates **in process** and emits `AgentSignal`
//! on its own channel — which is what makes it the proof that the non-file
//! path works end to end.
//!
//! Event mapping (§4.5):
//!
//! | OpenCode event | → |
//! |---|---|
//! | `permission.updated` | `Waiting` / `PermissionPrompt` |
//! | `session.idle` | `Idle` / `TurnComplete` |
//! | `message.part.updated`, `session.updated`, … | `Running` |
//!
//! Fidelity is **Exact**: `permission.updated` is a real, out-of-band "I am
//! blocked on you" event, so this adapter may raise an OS notification (R11).
//!
//! *Caveat carried from the v3.1 research:* the event names were confirmed from
//! the SDK's `event.subscribe` shape rather than enumerated in the docs, so
//! [`classify`] is written to degrade — an unrecognized event means "the agent
//! is doing something", never a false `Waiting`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

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

pub struct OpenCodeRuntime {
    worktree_root: PathBuf,
    host: String,
    port: u16,
    state_dir: Option<PathBuf>,
    /// Set on drop so the subscription thread stops reconnecting.
    stopped: Arc<AtomicBool>,
}

impl OpenCodeRuntime {
    pub fn new(worktree_root: impl Into<PathBuf>) -> Self {
        OpenCodeRuntime::with_endpoint(worktree_root, "127.0.0.1", DEFAULT_PORT)
    }

    /// Point at a specific endpoint (tests, or a user-configured port).
    pub fn with_endpoint(worktree_root: impl Into<PathBuf>, host: &str, port: u16) -> Self {
        OpenCodeRuntime {
            worktree_root: worktree_root.into(),
            host: host.to_string(),
            port,
            state_dir: None,
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
        // `permission.updated` is a real blocked-on-you event.
        WaitFidelity::Exact
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::Sse
    }

    fn on_spawn(&self, _worktree_root: &Path) -> std::io::Result<()> {
        if let Some(dir) = &self.state_dir {
            let shims = dir.join("shims");
            fs::create_dir_all(shims.join("opencode-remote"))?;
            for (name, content) in [
                ("opencode-remote/package.json", TUI_PACKAGE),
                ("opencode-remote/tui.js", TUI_PLUGIN),
                ("opencode-remote-launch.mjs", LAUNCHER),
            ] {
                let file = shims.join(name);
                if fs::read_to_string(&file).ok().as_deref() != Some(content) {
                    let tmp = file.with_extension("tmp");
                    fs::write(&tmp, content)?;
                    fs::rename(tmp, file)?;
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
            command.args.splice(..0, [launcher.to_owned(), program.to_owned()]);
            command.program = node;
        }
        command
    }

    fn subscribe(&self) -> Option<Receiver<AgentSignal>> {
        let (tx, rx) = unbounded();
        let host = self.host.clone();
        let port = self.port;
        let root = self.worktree_root.clone();
        let stopped = Arc::clone(&self.stopped);

        std::thread::Builder::new()
            .name("opencode-events".into())
            .spawn(move || pump(&host, port, &root, &tx, &stopped))
            .ok()?;
        Some(rx)
    }
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

/// Map an OpenCode event to a normalized state.
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
