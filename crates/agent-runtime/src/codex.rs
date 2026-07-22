//! The Codex and Kimi adapters (§4.5, roadmap A4) — the honest-degradation
//! cases.
//!
//! These two exist to prove that "terminal-only" and "best-effort" are
//! *supported first-class outcomes*, not failures (R11). Both agents run fully
//! in the embedded terminal; what differs is only how much we can say about
//! their state.
//!
//! **Codex — best-effort.** `config.toml` can name a `notify` program, but the
//! only event it receives in TUI mode is `agent-turn-complete`. There is **no
//! out-of-band approval-blocked event**, so this adapter can say "the turn
//! finished" and nothing more. It therefore reports
//! [`WaitFidelity::BestEffort`], which means `should_notify` refuses to raise a
//! "needs input" notification for it — labelling turn-complete as "input
//! needed" is exactly the lie R11 exists to prevent. (`codex exec --json` and
//! `codex mcp` expose exact events and are the upgrade path.)
//!
//! **Kimi — terminal-only.** The TUI exposes no hook at all, so the adapter
//! offers no signal and raises no notification. `kimi acp` speaks the Agent
//! Client Protocol (including permission requests) and is the structured
//! upgrade path when the TUI is not required.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity,
};

const SHIM: &str = include_str!("../shims/codex-notify.mjs");
const SHIM_FILENAME: &str = "codex-notify.mjs";

pub struct CodexRuntime {
    state_dir: PathBuf,
}

impl CodexRuntime {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        CodexRuntime {
            state_dir: state_dir.into(),
        }
    }

    pub fn shim_path(&self) -> PathBuf {
        self.state_dir.join("shims").join(SHIM_FILENAME)
    }

    fn materialize_shim(&self) -> io::Result<PathBuf> {
        let path = self.shim_path();
        if fs::read_to_string(&path).is_ok_and(|existing| existing == SHIM) {
            return Ok(path);
        }
        let dir = path.parent().expect("shim path always has a parent");
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{SHIM_FILENAME}.tmp"));
        fs::write(&tmp, SHIM)?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// The `notify` line Codex's `config.toml` needs.
    pub fn notify_line(shim: &Path) -> String {
        format!("notify = [\"node\", \"{}\"]\n", shim.display())
    }

    /// Where codex actually reads its config: `$CODEX_HOME`, else `~/.codex`.
    /// Overridable for tests via `HARNESS_CODEX_HOME`.
    fn codex_home(&self) -> PathBuf {
        if let Some(test) = std::env::var_os("HARNESS_CODEX_HOME") {
            return PathBuf::from(test);
        }
        if let Some(home) = std::env::var_os("CODEX_HOME") {
            return PathBuf::from(home);
        }
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".codex")
    }
}

impl AgentRuntime for CodexRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn fidelity(&self) -> WaitFidelity {
        // Turn-complete only — no approval signal exists in TUI mode (R11).
        WaitFidelity::BestEffort
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::Notify
    }

    fn ignored_paths(&self) -> &'static [&'static str] {
        &[".harness/", ".codex/"]
    }

    fn on_spawn(&self, _worktree_root: &Path) -> io::Result<()> {
        let shim = self.materialize_shim()?;
        // Codex reads ONE config: $CODEX_HOME/config.toml (default ~/.codex).
        // The project-local .codex/config.toml this adapter used to write was
        // never read by codex at all — the notify hook was dead on arrival.
        // Editing the user's global config is a bigger footprint, so two
        // guards keep it honest: the merge below preserves everything else in
        // the file, and the shim exits unless HARNESS_MANAGED=1 is in its
        // environment (set on the pty we spawn), so codex runs outside the
        // harness never write status files into random directories.
        let dir = self.codex_home();
        let path = dir.join("config.toml");

        // Preserve whatever else is in config.toml; replace only our own
        // notify line so repeated spawns stay idempotent.
        let existing = fs::read_to_string(&path).unwrap_or_default();
        let marker = shim.display().to_string();
        let mut kept: Vec<&str> = existing
            .lines()
            .filter(|l| !(l.trim_start().starts_with("notify") && l.contains(&marker)))
            .collect();
        // Drop a foreign `notify` too — Codex honors one, and ours must win or
        // the status pill silently never updates.
        kept.retain(|l| !l.trim_start().starts_with("notify"));

        let mut out = kept.join("\n");
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&Self::notify_line(&shim));

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".config.toml.tmp");
        fs::write(&tmp, out)?;
        fs::rename(&tmp, &path)
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        Some(AgentCommand {
            program: resolve_on_path("codex")?,
            args: Vec::new(),
            // The notify hook lives in the user's GLOBAL codex config; this
            // marker is what scopes it — the shim exits without writing
            // unless it inherits this from a harness-spawned codex.
            env: vec![("HARNESS_MANAGED".to_string(), "1".to_string())],
            label: "codex".to_string(),
        })
    }
}

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
            program: resolve_on_path("kimi")?,
            args: Vec::new(),
            env: Vec::new(),
            label: "kimi".to_string(),
        })
    }
}
