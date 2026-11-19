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

use crate::{resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity};

/// Everything materialized into the state dir, baked in at compile time.
///
/// `harness-status.ts` drives the sidebar pill (§4.1). `harness-tools.ts` pins
/// pi's runtime toolset to the versioned contract (§4.4, M6) — which it reads
/// from `tools.json`, so all three must be materialized together or the
/// extension reports a missing schema.
/// The default launch command — THE one definition for this agent. The spawn
/// below and the settings UI (via [`crate::default_command_display`]) both
/// derive from these two consts. The `-e <extension>` flags are adapter
/// wiring, not part of the user-facing default: they are per-machine absolute
/// paths added at spawn time.
pub(crate) const PROGRAM: &str = "pi";
pub(crate) const DEFAULT_ARGS: &[&str] = &[];

const ASSETS: &[(&str, &str)] = &[
    (
        "harness-status.ts",
        include_str!("../../../pi-ext/harness-status.ts"),
    ),
    (
        "harness-tools.ts",
        include_str!("../../../pi-ext/harness-tools.ts"),
    ),
    ("tools.json", agent_schema::TOOLS_JSON),
];

pub struct PiRuntime {
    /// Harness state root (`~/.openorchestra`), where the extensions are
    /// materialized once and shared by every worktree.
    state_dir: PathBuf,
}

impl PiRuntime {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        PiRuntime {
            state_dir: state_dir.into(),
        }
    }

    /// Directory the embedded assets are materialized into.
    pub fn asset_dir(&self) -> PathBuf {
        self.state_dir.join("pi-ext")
    }

    /// Where the status extension lives on disk once materialized.
    pub fn extension_path(&self) -> PathBuf {
        self.asset_dir().join("harness-status.ts")
    }

    /// Where the tool-contract extension lives.
    pub fn tools_extension_path(&self) -> PathBuf {
        self.asset_dir().join("harness-tools.ts")
    }

    /// Where the materialized tool contract lives (§4.4).
    pub fn schema_path(&self) -> PathBuf {
        self.asset_dir().join("tools.json")
    }

    /// Write every embedded asset that is missing or stale.
    ///
    /// Atomic (temp + rename) for the same reason the status extension's own
    /// writes are: a pi process may be starting up and reading these files
    /// concurrently, and a half-written extension is a load error rather than
    /// a no-op. Content-checked first, so the steady state is a read per file.
    fn materialize_assets(&self) -> io::Result<()> {
        let dir = self.asset_dir();
        for (name, contents) in ASSETS {
            let path = dir.join(name);
            if fs::read_to_string(&path).is_ok_and(|existing| existing == *contents) {
                continue; // already current — the common case
            }
            fs::create_dir_all(&dir)?;
            let tmp = dir.join(format!(".{name}.tmp"));
            fs::write(&tmp, contents)?;
            fs::rename(&tmp, &path)?;
        }
        Ok(())
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

    fn ignored_paths(&self) -> &'static [&'static str] {
        // The extension writes this; the extensions themselves live in the
        // state dir, outside the repo entirely.
        &[".harness/"]
    }

    fn on_spawn(&self, _worktree_root: &Path) -> io::Result<()> {
        self.materialize_assets()
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        let program = resolve_on_path(PROGRAM)?;
        // Materializing here as well as in on_spawn keeps `command` correct
        // when called standalone; it is a no-op once the files are current.
        self.materialize_assets().ok()?;
        Some(AgentCommand {
            program,
            args: vec![
                "-e".to_string(),
                self.extension_path().to_string_lossy().into_owned(),
                "-e".to_string(),
                self.tools_extension_path().to_string_lossy().into_owned(),
            ],
            // The packaged .app has no repo for the extension to walk up from,
            // so point it at the materialized contract explicitly (§4.4).
            env: vec![(
                "HARNESS_AGENT_SCHEMA".to_string(),
                self.schema_path().to_string_lossy().into_owned(),
            )],
            label: PROGRAM.to_string(),
        })
    }
}
