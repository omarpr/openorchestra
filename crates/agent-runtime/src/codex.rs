//! The Codex adapter (§4.5, roadmap A4) — the best-effort honest-degradation
//! case (its terminal-only sibling is [`crate::KimiRuntime`]).
//!
//! It exists to prove that "best-effort" is a *supported first-class outcome*,
//! not a failure (R11). The agent runs fully in the embedded terminal; the
//! adapter only limits how much we can say about its state.
//!
//! `config.toml` can name a `notify` program, but the only event it receives
//! in TUI mode is `agent-turn-complete`. There is **no out-of-band
//! approval-blocked event**, so this adapter can say "the turn finished" and
//! nothing more. It therefore reports [`WaitFidelity::BestEffort`], which
//! means `should_notify` refuses to raise a "needs input" notification for
//! it — labelling turn-complete as "input needed" is exactly the lie R11
//! exists to prevent. (`codex exec --json` and `codex mcp` expose exact
//! events and are the upgrade path.)

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity,
};

const SHIM: &str = include_str!("../shims/codex-notify.mjs");
const SHIM_FILENAME: &str = "codex-notify.mjs";

/// The default launch command — THE one definition for this agent. The spawn
/// below and the settings UI (via [`crate::default_command_display`]) both
/// derive from these two consts.
pub(crate) const PROGRAM: &str = "codex";
pub(crate) const DEFAULT_ARGS: &[&str] =
    &["--sandbox", "workspace-write", "--ask-for-approval", "never"];

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

/// Remove `notify` keys pointing at our shim from every table EXCEPT the
/// root (`allow` is true only at root, where the key belongs and will be
/// rewritten anyway). Cleans up configs damaged by the line-append bug.
fn repair_stray_notify(table: &mut toml_edit::Table, marker: &str, root: bool) {
    if !root
        && table
            .get("notify")
            .map(|v| v.to_string().contains(marker))
            .unwrap_or(false)
    {
        table.remove("notify");
    }
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        if let Some(child) = table.get_mut(&key).and_then(|i| i.as_table_mut()) {
            repair_stray_notify(child, marker, false);
        }
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
        // Editing the user's global config demands a real TOML editor: the
        // previous line-based append landed `notify` inside whatever [table]
        // happened to end the file — one user's config gained
        // `mcp_servers.node_repl.env.notify` and codex refused to start.
        // toml_edit assigns at the ROOT table and preserves formatting and
        // comments; the repair pass below also removes any stray notify keys
        // that bug left inside nested tables.
        let dir = self.codex_home();
        let path = dir.join("config.toml");

        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut doc: toml_edit::DocumentMut = existing.parse().map_err(|e| {
            // A config we cannot parse is one we must not rewrite.
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("codex config.toml does not parse; refusing to edit it: {e}"),
            )
        })?;

        let marker = shim.display().to_string();
        repair_stray_notify(doc.as_table_mut(), &marker, true);

        let mut arr = toml_edit::Array::new();
        arr.push("node");
        arr.push(marker.as_str());
        doc["notify"] = toml_edit::value(arr);

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".config.toml.tmp");
        fs::write(&tmp, doc.to_string())?;
        fs::rename(&tmp, &path)
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        Some(AgentCommand {
            program: resolve_on_path(PROGRAM)?,
            args: DEFAULT_ARGS.iter().map(|s| s.to_string()).collect(),
            // The notify hook lives in the user's GLOBAL codex config; this
            // marker is what scopes it — the shim exits without writing
            // unless it inherits this from a harness-spawned codex.
            env: vec![("HARNESS_MANAGED".to_string(), "1".to_string())],
            label: PROGRAM.to_string(),
        })
    }
}
