//! Configurable signal profile for custom (`Other`) agents.
//!
//! Rather than a Cursor-/aider-specific adapter, a custom agent declares a
//! [`SignalProfile`]: how much to trust its waiting signal, and optionally
//! which known hook dialect to install so a universal shim writes the shared
//! `.harness/status.json`. Any terminal agent that can invoke a command on
//! lifecycle edges can participate.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    default_invocation, resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind,
    WaitFidelity,
};

const SHIM: &str = include_str!("../shims/harness-status.mjs");
const SHIM_FILENAME: &str = "harness-status.mjs";

/// How (if at all) the harness installs hooks on spawn for a custom agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookInstall {
    /// User wires hooks themselves; settings UI shows the shim path.
    #[default]
    None,
    /// Merge Claude-compatible hooks into `<wt>/.claude/settings.local.json`.
    ClaudeLocalSettings,
    /// Merge Claude-compatible hooks into `~/.codex/hooks.json` (UserPromptSubmit + Stop).
    ///
    /// Each entry is gated on `$HARNESS_AGENT` matching this custom agent so the
    /// shared global file does not fire for built-in Codex (or other customs).
    CodexUserHooks,
}

/// Per-custom-agent signal configuration (persisted in prefs).
///
/// Absent from prefs (`None`) means terminal-only — today's default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignalProfile {
    pub fidelity: WaitFidelity,
    #[serde(default)]
    pub install: HookInstall,
}

impl SignalProfile {
    pub fn best_effort(install: HookInstall) -> Self {
        SignalProfile {
            fidelity: WaitFidelity::BestEffort,
            install,
        }
    }

    pub fn exact(install: HookInstall) -> Self {
        SignalProfile {
            fidelity: WaitFidelity::Exact,
            install,
        }
    }
}

/// Absolute path where the universal status shim is materialized.
pub fn harness_status_shim_path(state_dir: &Path) -> PathBuf {
    state_dir.join("shims").join(SHIM_FILENAME)
}

/// Write (or refresh) the universal status shim under `state_dir/shims/`.
pub fn materialize_harness_status_shim(state_dir: &Path) -> io::Result<PathBuf> {
    let path = harness_status_shim_path(state_dir);
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

/// Displayable invocation for Manual install: `node "<shim>"`.
pub fn harness_status_shim_command(state_dir: &Path) -> String {
    format!("node {:?}", harness_status_shim_path(state_dir).display().to_string())
}

/// Adapter for [`AgentKind::Other`] with a configured [`SignalProfile`].
pub struct ConfigurableRuntime {
    kind: AgentKind,
    state_dir: PathBuf,
    profile: SignalProfile,
}

impl ConfigurableRuntime {
    pub fn new(kind: AgentKind, state_dir: impl Into<PathBuf>, profile: SignalProfile) -> Self {
        ConfigurableRuntime {
            kind,
            state_dir: state_dir.into(),
            profile,
        }
    }

    pub fn profile(&self) -> &SignalProfile {
        &self.profile
    }

    pub fn shim_path(&self) -> PathBuf {
        harness_status_shim_path(&self.state_dir)
    }

    fn managed_env(&self) -> Vec<(String, String)> {
        vec![
            ("HARNESS_MANAGED".into(), "1".into()),
            ("HARNESS_AGENT".into(), self.kind.as_str().to_string()),
        ]
    }

    /// Claude-local settings hook block pointing at our universal shim.
    pub fn claude_settings_for(shim: &Path) -> Value {
        let command = format!("node {}", shim.display());
        let entry = || {
            json!({
                "hooks": [{ "type": "command", "command": command }]
            })
        };
        json!({
            "hooks": {
                "UserPromptSubmit": [entry()],
                "PreToolUse": [entry()],
                "Notification": [entry()],
                "Stop": [entry()],
            }
        })
    }

    fn install_claude_local(&self, worktree_root: &Path, shim: &Path) -> io::Result<()> {
        let dir = worktree_root.join(".claude");
        let path = dir.join("settings.local.json");

        let mut root: Value = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| json!({}));

        let ours = Self::claude_settings_for(shim);
        let hooks = root
            .as_object_mut()
            .expect("settings root is an object")
            .entry("hooks")
            .or_insert_with(|| json!({}));

        if let (Some(hooks), Some(ours)) = (hooks.as_object_mut(), ours["hooks"].as_object()) {
            let shim_marker = shim.display().to_string();
            for (event, entries) in ours {
                let list = hooks
                    .entry(event.clone())
                    .or_insert_with(|| json!([]));
                let Some(list) = list.as_array_mut() else {
                    continue;
                };
                list.retain(|e| !serde_json::to_string(e).unwrap_or_default().contains(&shim_marker));
                if let Some(add) = entries.as_array() {
                    list.extend(add.iter().cloned());
                }
            }
        }

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".settings.local.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&root)?)?;
        fs::rename(&tmp, &path)
    }

    fn codex_home() -> PathBuf {
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

    /// Shell command for a Codex global hook: no-op unless `$HARNESS_AGENT`
    /// matches this custom agent. Built-in Codex only sets `HARNESS_MANAGED`, so
    /// leftover entries here never write status (or race the codex-hook shim).
    ///
    /// The agent slug is left unquoted: prefs only allow `[A-Za-z0-9_-]`, and a
    /// quoted value would become `\"…\"` inside `Value::to_string`, breaking the
    /// substring marker used for idempotent reinstalls.
    fn codex_hook_command(shim: &Path, agent: &str) -> String {
        format!(
            "[ \"$HARNESS_AGENT\" = {agent} ] && node {:?}",
            shim.display().to_string()
        )
    }

    fn install_codex_hooks(&self, shim: &Path) -> io::Result<()> {
        let dir = Self::codex_home();
        let path = dir.join("hooks.json");

        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut root: Value = if existing.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("codex hooks.json does not parse; refusing to edit it: {e}"),
                )
            })?
        };
        if !root.is_object() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "codex hooks.json is not a JSON object; refusing to edit it",
            ));
        }

        let agent = self.kind.as_str();
        // Marker is per-agent (not just the shared shim path) so two customs
        // and the built-in `codex-hook.mjs` entry can coexist without clobbering.
        let marker = format!("= {agent} ] && node");
        let command = Self::codex_hook_command(shim, agent);
        for event in ["UserPromptSubmit", "Stop"] {
            ensure_hook_group(&mut root, event, &marker, &command);
        }

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".hooks.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&root)? + "\n")?;
        fs::rename(&tmp, &path)
    }
}

fn ensure_hook_group(root: &mut Value, event: &str, marker: &str, command: &str) {
    let hooks = root
        .as_object_mut()
        .expect("caller verified root is an object")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let arr = hooks
        .as_object_mut()
        .unwrap()
        .entry(event)
        .or_insert_with(|| json!([]));
    if !arr.is_array() {
        *arr = json!([]);
    }
    let arr = arr.as_array_mut().unwrap();
    arr.retain(|group| !group.to_string().contains(marker));
    arr.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
}

impl AgentRuntime for ConfigurableRuntime {
    fn kind(&self) -> AgentKind {
        self.kind.clone()
    }

    fn fidelity(&self) -> WaitFidelity {
        self.profile.fidelity
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::StatusFile
    }

    fn ignored_paths(&self) -> &'static [&'static str] {
        match self.profile.install {
            HookInstall::None => &[".harness/"],
            HookInstall::ClaudeLocalSettings => &[".harness/", ".claude/settings.local.json"],
            HookInstall::CodexUserHooks => &[".harness/", ".codex/"],
        }
    }

    fn on_spawn(&self, worktree_root: &Path) -> io::Result<()> {
        let shim = materialize_harness_status_shim(&self.state_dir)?;
        match self.profile.install {
            HookInstall::None => Ok(()),
            HookInstall::ClaudeLocalSettings => self.install_claude_local(worktree_root, &shim),
            HookInstall::CodexUserHooks => self.install_codex_hooks(&shim),
        }
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        let (program, args) = default_invocation(&self.kind);
        Some(AgentCommand {
            program: resolve_on_path(program).unwrap_or_else(|| PathBuf::from(program)),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: self.managed_env(),
            label: program.to_string(),
        })
    }
}
