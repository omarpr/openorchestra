//! The pi adapter — the reference runtime (§4.4, §4.5).
//!
//! pi is the one agent we train ourselves and can hack end to end, so for pi
//! the harness conventions and the training data are the same thing. It is also
//! the only adapter whose signal is *first-party*: `harness-status.ts`
//! subscribes to pi's own lifecycle events and writes `.harness/status.json`.
//!
//! **The extension is embedded in the binary** (`include_str!`) rather than
//! read from the source tree, so a packaged `openOrchestra.app` — which has no
//! checkout to read from — behaves identically to a dev build.
//!
//! **It is passed with `pi -e <path>`, not copied into each worktree's
//! `.pi/extensions/`.** §4.1 originally described the copy; `-e` is an equally
//! supported pi mechanism (docs/spike2-notes.md §1) and is strictly better
//! here: it keeps a `.pi/` directory out of every worktree, so the harness
//! doesn't add untracked files to the user's repo, and it means an extension
//! upgrade takes effect on the next spawn everywhere instead of leaving one
//! stale copy per worktree.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity,
};

/// The status extension source, baked in at compile time.
const HARNESS_STATUS_EXT: &str = include_str!("../../../pi-ext/harness-status.ts");

/// Filename the extension is materialized under, inside the state dir.
const EXT_FILENAME: &str = "harness-status.ts";

pub struct PiRuntime {
    /// Harness state root (`~/.openorchestra`), where the extension is
    /// materialized once and shared by every worktree.
    state_dir: PathBuf,
}

impl PiRuntime {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        PiRuntime {
            state_dir: state_dir.into(),
        }
    }

    /// Where the extension lives on disk once materialized.
    pub fn extension_path(&self) -> PathBuf {
        self.state_dir.join("pi-ext").join(EXT_FILENAME)
    }

    /// Write the embedded extension to disk if it is missing or stale.
    ///
    /// Atomic (temp + rename) for the same reason the extension's own writes
    /// are: a pi process may be starting up and reading this file concurrently,
    /// and a half-written extension is a load error rather than a no-op.
    fn materialize_extension(&self) -> io::Result<PathBuf> {
        let path = self.extension_path();
        if fs::read_to_string(&path).is_ok_and(|existing| existing == HARNESS_STATUS_EXT) {
            return Ok(path); // already current — the common case
        }
        let dir = path.parent().expect("extension path always has a parent");
        fs::create_dir_all(dir)?;
        let tmp = dir.join(format!(".{EXT_FILENAME}.tmp"));
        fs::write(&tmp, HARNESS_STATUS_EXT)?;
        fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

impl AgentRuntime for PiRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Pi
    }

    fn fidelity(&self) -> WaitFidelity {
        // The extension wraps the shared uiContext, so it sees dialogs opened
        // through the extension UI — the approval path. It cannot see pi-core's
        // own TUI dialogs (R8), and `agent_settled` is approximated from
        // agent_end + a debounce. Real `Exact` arrives with Phase 2's RPC
        // extension_ui_request (M8).
        WaitFidelity::BestEffort
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::StatusFile
    }

    fn on_spawn(&self, _worktree_root: &Path) -> io::Result<()> {
        self.materialize_extension().map(|_| ())
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        let program = resolve_on_path("pi")?;
        // Materializing here as well as in on_spawn keeps `command` correct
        // when called standalone; it is a no-op once the file is current.
        let ext = self.materialize_extension().ok()?;
        Some(AgentCommand {
            program,
            args: vec!["-e".to_string(), ext.to_string_lossy().into_owned()],
            env: Vec::new(),
            label: "pi".to_string(),
        })
    }
}
