//! The Grok adapter.
//!
//! Grok's hooks are Claude-compatible. A project-local
//! `.grok/hooks/openorchestra.json` names a command for the same lifecycle
//! events Claude Code uses, and the shim writes the shared
//! `.harness/status.json`. `status-watch` consumes it unchanged.
//!
//! **The hook file is machine-local.** The command embeds an absolute path to
//! a shim under the harness state dir, so it is wrong on anyone else's
//! checkout. It is the one file this adapter writes, and the app adds that
//! path to the repo's local excludes — the same treatment as Claude Code's
//! `settings.local.json`. Grok's other hook files are left alone.
//!
//! Project hooks run only after the folder is trusted. This adapter does not
//! pass `--trust`: that grant also turns on the checkout's MCP servers and
//! project instructions. Until the user trusts the folder, Grok still runs;
//! the status pill stays quiet.
//!
//! **Licensing:** Grok is the user's own installed CLI, resolved from `$PATH`
//! and spawned as a separate process. Nothing is bundled or linked.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity};

/// The hook shim, materialized next to the other agent assets.
const SHIM: &str = include_str!("../shims/grok-status.mjs");
/// `grok-status.mjs` imports the shared classifier from this sibling file.
const CLASSIFY_SHIM: &str = include_str!("../shims/claude-code-status.mjs");

const SHIM_FILENAME: &str = "grok-status.mjs";
const CLASSIFY_FILENAME: &str = "claude-code-status.mjs";

/// The default launch command — the one definition for this agent. The
/// registry kind is "grok"; the binary is `grok`.
pub(crate) const PROGRAM: &str = "grok";
pub(crate) const DEFAULT_ARGS: &[&str] = &["--permission-mode", "auto"];

/// Gitexclude path for the hook file this adapter writes.
const HOOK_EXCLUDE: &str = ".grok/hooks/openorchestra.json";

pub struct GrokRuntime {
    state_dir: PathBuf,
}

impl GrokRuntime {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        GrokRuntime {
            state_dir: state_dir.into(),
        }
    }

    pub fn shim_path(&self) -> PathBuf {
        self.state_dir.join("shims").join(SHIM_FILENAME)
    }

    pub fn materialize_shim(&self) -> io::Result<PathBuf> {
        crate::install_remote_tab_helper(&self.state_dir)?;
        // The grok shim imports the classifier, so both files have to land
        // side by side. Writing the classifier here is idempotent with the
        // Claude Code adapter, which materializes the same bytes.
        install_shim(
            &self.state_dir.join("shims").join(CLASSIFY_FILENAME),
            CLASSIFY_SHIM,
        )?;
        install_shim(&self.shim_path(), SHIM)
    }

    /// Hook block for a shim at `shim`. No `matcher`: on Notification a
    /// matcher filters the event out, which is how Claude Code's waiting
    /// signal was lost.
    pub fn hooks_for(shim: &Path) -> serde_json::Value {
        let command = crate::node_command(shim);
        let entry = || {
            serde_json::json!({
                "hooks": [{ "type": "command", "command": command }]
            })
        };
        serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [entry()],
                "PreToolUse": [entry()],
                "Notification": [entry()],
                "Stop": [entry()],
            }
        })
    }

    /// Merge our hooks into `.grok/hooks/openorchestra.json`.
    ///
    /// The file is ours, but a person may have added other events to it.
    /// Replace only entries that point at this shim.
    fn write_hooks(&self, worktree_root: &Path, shim: &Path) -> io::Result<()> {
        let dir = worktree_root.join(".grok").join("hooks");
        let path = dir.join("openorchestra.json");

        let mut root: serde_json::Value = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        let ours = Self::hooks_for(shim);
        let hooks = root
            .as_object_mut()
            .expect("hook file root is an object")
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        if let (Some(hooks), Some(ours)) = (hooks.as_object_mut(), ours["hooks"].as_object()) {
            for (event, entries) in ours {
                let list = hooks
                    .entry(event.clone())
                    .or_insert_with(|| serde_json::json!([]));
                let Some(list) = list.as_array_mut() else {
                    continue;
                };
                let shim_marker = shim.display().to_string();
                list.retain(|e| {
                    !crate::mentions_shim(&serde_json::to_string(e).unwrap_or_default(), &shim_marker)
                });
                if let Some(add) = entries.as_array() {
                    list.extend(add.iter().cloned());
                }
            }
        }

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".openorchestra.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
        fs::rename(&tmp, &path)
    }
}

fn install_shim(target: &Path, contents: &str) -> io::Result<PathBuf> {
    if fs::read_to_string(target).is_ok_and(|existing| existing == contents) {
        return Ok(target.to_path_buf());
    }
    let dir = target.parent().expect("shim path always has a parent");
    fs::create_dir_all(dir)?;
    let name = target
        .file_name()
        .and_then(|n| n.to_str())
        .expect("shim file name is utf-8");
    let tmp = dir.join(format!(".{name}.tmp"));
    fs::write(&tmp, contents)?;
    fs::rename(&tmp, target)?;
    Ok(target.to_path_buf())
}

impl AgentRuntime for GrokRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Grok
    }

    fn fidelity(&self) -> WaitFidelity {
        WaitFidelity::Exact
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::StatusFile
    }

    fn ignored_paths(&self) -> &'static [&'static str] {
        &[".harness/", HOOK_EXCLUDE]
    }

    fn on_spawn(&self, worktree_root: &Path) -> io::Result<()> {
        let shim = self.materialize_shim()?;
        self.write_hooks(worktree_root, &shim)
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
