//! agent-runtime — the agent-agnostic layer (architecture §4.5–4.6, D12).
//!
//! **The substrate is the terminal.** Running an agent means spawning that
//! vendor's own CLI in the worktree's pty, exactly as the harness already does
//! for pi. Every agent works that way; nothing here is required for an agent to
//! be usable.
//!
//! What this crate adds on top is the *optional* enhancement: an
//! [`AgentRuntime`] per vendor that knows (a) which command to run plus any
//! per-agent config to drop in, and (b) — where the agent exposes a signal
//! surface — how that agent's native "needs input" event normalizes into the
//! same three states the sidebar already consumes.
//!
//! Signal fidelity is deliberately heterogeneous and reported honestly via
//! [`WaitFidelity`] (R11): an agent with no signal surface still runs fully in
//! its terminal, it just shows a coarser status and raises no notification.
//!
//! UI-free like `worktrees` / `forge` / `status-watch`, so the same layer can
//! drive batch evals with no window.

mod bus;
mod claude_code;
mod codex;
mod opencode;
mod pi;
pub mod sse;
mod status_file;

pub use bus::Bus;
pub use claude_code::ClaudeCodeRuntime;
pub use codex::{CodexRuntime, KimiRuntime};
pub use opencode::OpenCodeRuntime;
pub use pi::PiRuntime;
pub use status_file::StatusFileSource;

use std::path::{Path, PathBuf};

pub use status_watch::AgentState;
pub use worktrees::AgentKind;

/// Why an agent is waiting, when it tells us (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// Blocked on an approval / permission prompt.
    PermissionPrompt,
    /// Blocked at an idle prompt, waiting for the next instruction.
    IdlePrompt,
    /// Not blocked — the turn simply finished.
    TurnComplete,
    None,
}

/// How much to trust an agent's "waiting" signal (R11). The UI reflects this
/// rather than pretending every agent reports the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitFidelity {
    /// A real, out-of-band "I am blocked on you" event.
    Exact,
    /// Only coarser evidence — e.g. turn-complete, but no approval signal.
    BestEffort,
    /// No signal surface at all; the terminal is the whole story.
    TerminalOnly,
}

/// How an adapter's enhancement arrives, if at all (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Writes the shared `.harness/status.json` (pi, and Claude Code via a hook
    /// shim). Consumed by [`StatusFileSource`], not by the adapter itself.
    StatusFile,
    /// Server-sent events from the agent's local HTTP server (OpenCode).
    Sse,
    /// A `notify` program the agent invokes (Codex).
    Notify,
    /// Agent Client Protocol over JSON-RPC (Kimi via `kimi acp`).
    Acp,
    /// Terminal-only — a supported first-class case, not a failure.
    None,
}

/// One normalized status update for a worktree, from any adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSignal {
    pub worktree_root: PathBuf,
    pub kind: AgentKind,
    pub state: AgentState,
    pub reason: WaitReason,
    /// Path to the agent's session transcript, when the adapter exposes one —
    /// the harvesting hook for §6.4 / M7.
    pub session_file: Option<PathBuf>,
}

impl AgentSignal {
    /// Whether this transition should raise a native "needs input"
    /// notification (§4.6). Only an exact signal may claim the user is
    /// actually blocked — a best-effort adapter reporting turn-complete must
    /// not be dressed up as "input needed".
    pub fn should_notify(&self, fidelity: WaitFidelity) -> bool {
        self.state == AgentState::Waiting && fidelity == WaitFidelity::Exact
    }
}

/// The command to run in the worktree's pty.
///
/// Deliberately data, not a `std::process::Command`: the terminal pane builds
/// its own pty spawn from this, and tests can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand {
    /// Absolute path to the resolved binary.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Short label for the tab / status bar (e.g. `"pi"`).
    pub label: String,
}

/// One vendor's adapter (§4.5).
///
/// The harness bundles, redistributes, and links none of these agents — each is
/// the user's own installed CLI resolved from `$PATH` and run as a separate
/// subprocess, so there is no license coupling and no ToS concern (R12).
pub trait AgentRuntime: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// How much to trust this adapter's `Waiting` (R11).
    fn fidelity(&self) -> WaitFidelity;

    /// How the optional enhancement arrives, if at all.
    fn signal_kind(&self) -> SignalKind;

    /// Paths this adapter writes into the worktree, for the caller to add to
    /// the repo's local git excludes.
    ///
    /// The harness runs inside the user's own repositories, so anything it
    /// drops there would otherwise show up as uncommitted changes in their
    /// project — and in Claude Code's case carry a machine-specific absolute
    /// path that must never be committed.
    fn ignored_paths(&self) -> &'static [&'static str] {
        &[]
    }

    /// Prepare the worktree before spawning: drop in whatever per-agent config
    /// enables the signal surface (a pi extension, a `.claude/settings.json`
    /// hook, a `notify` entry). Must be idempotent — it runs on every spawn.
    ///
    /// A failure here is never fatal: the agent still runs in its terminal, it
    /// just loses the enhancement. Callers should log and continue.
    fn on_spawn(&self, worktree_root: &Path) -> std::io::Result<()>;

    /// The CLI to run in the pty, or `None` when the binary is not installed —
    /// in which case the caller falls back to a plain shell rather than
    /// failing, since a terminal is still useful.
    fn command(&self, worktree_root: &Path) -> Option<AgentCommand>;

    /// A stream of signals this adapter produces itself.
    ///
    /// `None` for adapters whose signal arrives through a shared source instead
    /// (anything with [`SignalKind::StatusFile`] — see [`StatusFileSource`]),
    /// and for terminal-only agents.
    fn subscribe(&self) -> Option<crossbeam_channel::Receiver<AgentSignal>> {
        None
    }
}

/// Find an executable named `program` on `$PATH`.
///
/// Deliberately does not consult `$SHELL` or a bundled copy: §4.5/R12 rests on
/// this being *the user's own installed, authenticated CLI*.
pub fn resolve_on_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Pick the adapter for a worktree's recorded [`AgentKind`].
///
/// Unknown kinds (a registry written by a newer build) fall back to a
/// terminal-only adapter that runs the kind's own name as the command — the
/// honest behavior: we can still launch it, we just have no signal.
pub fn runtime_for(
    kind: &AgentKind,
    worktree_root: &Path,
    state_dir: &Path,
) -> Box<dyn AgentRuntime> {
    match kind {
        AgentKind::Pi => Box::new(PiRuntime::new(state_dir)),
        AgentKind::OpenCode => Box::new(OpenCodeRuntime::new(worktree_root)),
        AgentKind::ClaudeCode => Box::new(ClaudeCodeRuntime::new(state_dir)),
        AgentKind::Codex => Box::new(CodexRuntime::new(state_dir)),
        AgentKind::Kimi => Box::new(KimiRuntime),
        other => Box::new(TerminalOnlyRuntime::new(other.clone())),
    }
}

/// The baseline adapter: run the agent's CLI, offer no signal.
///
/// This is what every agent gets before its dedicated adapter exists, and what
/// agents with no signal surface (Kimi's TUI) get permanently.
pub struct TerminalOnlyRuntime {
    kind: AgentKind,
}

impl TerminalOnlyRuntime {
    pub fn new(kind: AgentKind) -> Self {
        TerminalOnlyRuntime { kind }
    }
}

impl AgentRuntime for TerminalOnlyRuntime {
    fn kind(&self) -> AgentKind {
        self.kind.clone()
    }

    fn fidelity(&self) -> WaitFidelity {
        WaitFidelity::TerminalOnly
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::None
    }

    fn on_spawn(&self, _worktree_root: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        // The CLI name is the kind's wire string for every agent in §4.5:
        // pi / opencode / codex / claude-code -> `claude` / kimi.
        let program = match self.kind {
            AgentKind::ClaudeCode => "claude",
            _ => self.kind.as_str(),
        };
        Some(AgentCommand {
            program: resolve_on_path(program)?,
            args: Vec::new(),
            env: Vec::new(),
            label: program.to_string(),
        })
    }
}

/// Re-export of the OpenCode event classifier, so the mapping table in §4.5
/// can be asserted directly without exposing the whole module.
pub use opencode::classify as opencode_classify;
