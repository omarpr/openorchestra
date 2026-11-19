//! Configurable signal profile for custom (`Other`) agents.
//!
//! A custom agent declares a
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
    /// Inject status hooks for supported agent identities at launch.
    Auto,
    /// Merge Claude-compatible hooks into `<wt>/.claude/settings.local.json`.
    ClaudeLocalSettings,
    /// Merge Claude-compatible hooks into `~/.codex/hooks.json` (UserPromptSubmit + Stop).
    ///
    /// Each entry is gated on `$HARNESS_AGENT` matching this custom agent so the
    /// shared global file does not fire for built-in Codex (or other customs).
    CodexUserHooks,
    /// Merge Cursor lifecycle hooks into the user's `~/.cursor/hooks.json`.
    CursorUserHooks,
}

impl HookInstall {
    pub fn effective(self, agent: &str) -> Self {
        if self != Self::Auto {
            return self;
        }
        match agent {
            "claude" | "claude-code" => Self::ClaudeLocalSettings,
            "codex" => Self::CodexUserHooks,
            "cursor" | "cursor-agent" => Self::CursorUserHooks,
            _ => Self::None,
        }
    }
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
    crate::install_remote_tab_helper(state_dir)?;
    let path = harness_status_shim_path(state_dir);
    crate::install_shim(&path, SHIM)?;
    Ok(path)
}

/// Displayable invocation for Manual install: `node "<shim>"`.
pub fn harness_status_shim_command(state_dir: &Path) -> String {
    format!(
        "node {:?}",
        harness_status_shim_path(state_dir).display().to_string()
    )
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
                let list = hooks.entry(event.clone()).or_insert_with(|| json!([]));
                let Some(list) = list.as_array_mut() else {
                    continue;
                };
                list.retain(|e| {
                    !serde_json::to_string(e)
                        .unwrap_or_default()
                        .contains(&shim_marker)
                });
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
    /// Custom agent slugs from preferences use `[A-Za-z0-9_-]`.
    fn codex_hook_command(shim: &Path, agent: &str) -> String {
        format!(
            "[ \"$HARNESS_AGENT\" = {agent} ] && node {:?}",
            shim.display().to_string()
        )
    }

    fn install_codex_hooks(&self, shim: &Path) -> io::Result<()> {
        let agent = self.kind.as_str();
        let marker = format!("= {agent} ] && node");
        let command = Self::codex_hook_command(shim, agent);
        crate::codex::install_status_hooks(&Self::codex_home(), &marker, &command)
    }

    fn install_cursor_hooks(&self, shim: &Path) -> io::Result<()> {
        let home = std::env::var_os("HARNESS_CURSOR_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cursor")))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Cursor config directory is unavailable",
                )
            })?;
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let marker = format!(
            "if [ \"$HARNESS_AGENT\" = {} ]; then node ",
            quote(self.kind.as_str())
        );
        let command = format!("{marker}{}; fi", quote(&shim.to_string_lossy()));
        crate::update_agent_hooks(&home, |root| {
            let root = root.as_object_mut().expect("validated hooks object");
            root.entry("version").or_insert(json!(1));
            let hooks = root
                .entry("hooks")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "Cursor hooks must be an object")
                })?;
            for event in ["beforeSubmitPrompt", "stop"] {
                let handlers = hooks
                    .entry(event)
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("Cursor {event} hooks must be an array"),
                        )
                    })?;
                if let Some(handler) = handlers.iter_mut().find(|handler| {
                    handler["command"]
                        .as_str()
                        .is_some_and(|text| text.starts_with(&marker))
                }) {
                    handler["command"] = json!(command);
                } else {
                    handlers.push(json!({ "command": command }));
                }
            }
            Ok(())
        })
    }
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
        match self.profile.install.effective(self.kind.as_str()) {
            HookInstall::None | HookInstall::Auto | HookInstall::CursorUserHooks => &[".harness/"],
            HookInstall::ClaudeLocalSettings => &[".harness/", ".claude/settings.local.json"],
            HookInstall::CodexUserHooks => &[".harness/", ".codex/"],
        }
    }

    fn on_spawn(&self, worktree_root: &Path) -> io::Result<()> {
        let shim = materialize_harness_status_shim(&self.state_dir)?;
        match self.profile.install.effective(self.kind.as_str()) {
            HookInstall::None | HookInstall::Auto => Ok(()),
            HookInstall::ClaudeLocalSettings => self.install_claude_local(worktree_root, &shim),
            HookInstall::CodexUserHooks => self.install_codex_hooks(&shim),
            HookInstall::CursorUserHooks => self.install_cursor_hooks(&shim),
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
