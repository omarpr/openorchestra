//! The Kimi adapter (§4.5, roadmap A4) — the terminal-only honest-degradation
//! case (its best-effort sibling is [`crate::CodexRuntime`]).
//!
//! It exists to prove that "terminal-only" is a *supported first-class
//! outcome*, not a failure (R11). The agent runs fully in the embedded
//! terminal; the TUI exposes no hook at all, so the adapter offers no signal
//! and raises no notification. `kimi acp` speaks the Agent Client Protocol
//! (including permission requests) and is the structured upgrade path when
//! the TUI is not required.

use std::io;
use std::path::Path;

use crate::{resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity};

/// The default launch command — THE one definition for this agent. The spawn
/// below and the settings UI (via [`crate::default_command_display`]) both
/// derive from these two consts.
pub(crate) const PROGRAM: &str = "kimi";
pub(crate) const DEFAULT_ARGS: &[&str] = &[];

/// Kimi: runs in the terminal, offers no signal from the TUI.
///
/// A distinct type rather than a reuse of `TerminalOnlyRuntime` so that the
/// ACP upgrade path (`kimi acp`) has somewhere to land.
pub struct KimiRuntime;

impl AgentRuntime for KimiRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Kimi
    }

    fn fidelity(&self) -> WaitFidelity {
        WaitFidelity::TerminalOnly
    }

    fn signal_kind(&self) -> SignalKind {
        // `kimi acp` would make this SignalKind::Acp; the TUI has nothing.
        SignalKind::None
    }

    fn on_spawn(&self, _worktree_root: &Path) -> io::Result<()> {
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
}
