//! The Claude Code adapter (§4.5, roadmap A3).
//!
//! Claude Code exposes `Notification` hooks: a project-local
//! `.claude/settings.json` can name a command to run when the agent needs
//! permission, goes idle at a prompt, or stops. We write that settings file and
//! point it at a **shim** that writes the shared `.harness/status.json` — so
//! Claude Code normalizes through exactly the same file pi does, and the
//! existing `status-watch` consumes it unchanged (§4.5).
//!
//! The hook payload carries `transcript_path`, which the shim records as
//! `sessionFile` — making Claude Code the one *peer* agent that is harvestable
//! (§6.4), though the training loop stays pi-first (§4.4).
//!
//! **Licensing (R12):** Claude Code is proprietary, and that is a non-issue
//! here by construction. We spawn the user's own installed, authenticated
//! `claude` from `$PATH` as a separate subprocess and run none of its code in
//! process — exactly as a user typing `claude` in Terminal.app would. Nothing
//! is bundled, redistributed, or linked.
//!
//! **Hook-schema drift (R12 residual):** the hook schema moved across 2.1.x, so
//! the shim is written to be tolerant — it reads several spellings of each
//! field and falls back to a safe state rather than failing, since a shim that
//! errors would make the agent look broken.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity,
};

/// The hook shim, materialized next to the other agent assets.
///
/// Node is already a hard requirement for pi (R4), so reusing it here avoids
/// adding a second scripting runtime to the dependency surface.
const SHIM: &str = include_str!("../shims/claude-code-status.mjs");

const SHIM_FILENAME: &str = "claude-code-status.mjs";

pub struct ClaudeCodeRuntime {
    state_dir: PathBuf,
}

impl ClaudeCodeRuntime {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        ClaudeCodeRuntime {
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

    /// Build the `.claude/settings.json` hook block for a shim at `shim`.
    ///
    /// Public so the test suite can assert the exact shape we write without
    /// reaching into the filesystem.
    pub fn settings_for(shim: &Path) -> serde_json::Value {
        let command = format!("node {}", shim.display());
        let entry = |matcher: &str| {
            serde_json::json!({
                "matcher": matcher,
                "hooks": [{ "type": "command", "command": command }]
            })
        };
        serde_json::json!({
            "hooks": {
                // permission_prompt / idle_prompt are the two "blocked on the
                // user" matchers; Stop closes the turn out to idle.
                "Notification": [entry("permission_prompt"), entry("idle_prompt")],
                "Stop": [entry("")],
            }
        })
    }

    /// Merge our hooks into the worktree's `.claude/settings.json`.
    ///
    /// Merge rather than overwrite: the user may have their own hooks and
    /// settings there, and clobbering them would be a hostile thing for a
    /// harness to do to a project directory.
    fn write_settings(&self, worktree_root: &Path, shim: &Path) -> io::Result<()> {
        let dir = worktree_root.join(".claude");
        let path = dir.join("settings.json");

        let mut root: serde_json::Value = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| serde_json::json!({}));

        let ours = Self::settings_for(shim);
        let hooks = root
            .as_object_mut()
            .expect("settings root is an object")
            .entry("hooks")
            .or_insert_with(|| serde_json::json!({}));

        if let (Some(hooks), Some(ours)) = (hooks.as_object_mut(), ours["hooks"].as_object()) {
            for (event, entries) in ours {
                // Replace only the entries pointing at *our* shim, so repeated
                // spawns stay idempotent without dropping the user's hooks.
                let list = hooks
                    .entry(event.clone())
                    .or_insert_with(|| serde_json::json!([]));
                let Some(list) = list.as_array_mut() else {
                    continue;
                };
                let shim_marker = shim.display().to_string();
                list.retain(|e| !serde_json::to_string(e).unwrap_or_default().contains(&shim_marker));
                if let Some(add) = entries.as_array() {
                    list.extend(add.iter().cloned());
                }
            }
        }

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".settings.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
        fs::rename(&tmp, &path)
    }
}

impl AgentRuntime for ClaudeCodeRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }

    fn fidelity(&self) -> WaitFidelity {
        // `permission_prompt` is a real blocked-on-you notification.
        WaitFidelity::Exact
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::StatusFile
    }

    fn on_spawn(&self, worktree_root: &Path) -> io::Result<()> {
        let shim = self.materialize_shim()?;
        self.write_settings(worktree_root, &shim)
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        // The registry kind is "claude-code"; the binary is `claude`.
        Some(AgentCommand {
            program: resolve_on_path("claude")?,
            args: Vec::new(),
            env: Vec::new(),
            label: "claude".to_string(),
        })
    }
}
