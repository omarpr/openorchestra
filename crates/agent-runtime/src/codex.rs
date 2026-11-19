//! The Codex adapter (§4.5, roadmap A4).
//!
//! Codex runs fully in the embedded terminal; the adapter's job is to keep the
//! sidebar pill honest about whether it is working or idle.
//!
//! **Status via hooks, not `notify`.** The old path hooked codex's `notify`
//! program, which in TUI mode only ever fired `agent-turn-complete` — turn
//! END, never turn START. So nothing wrote "running" for the second and later
//! turns and the pill was stuck on IDLE while codex was plainly working.
//! Codex 0.145 grew a Claude-Code-style hooks system, which gives us both
//! edges: `UserPromptSubmit` (turn started -> running) and `Stop` (turn
//! finished -> idle). We install those two into the user's global
//! `~/.codex/hooks.json`; the shim writes the same three-state
//! `.harness/status.json` every other adapter uses, so `StatusFileSource`
//! consumes it unchanged.
//!
//! Fidelity stays [`WaitFidelity::BestEffort`]. The hooks we install track
//! turn boundaries. An approval prompt can appear during a turn without
//! changing the reported status.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{json, Value};

use crate::{
    resolve_on_path, AgentCommand, AgentKind, AgentRuntime, SignalKind, WaitFidelity,
};

const SHIM: &str = include_str!("../shims/codex-hook.mjs");
const SHIM_FILENAME: &str = "codex-hook.mjs";
/// The pre-hooks status shim. on_spawn strips its `notify` line from
/// `config.toml` so the two mechanisms don't both write `status.json`.
const LEGACY_NOTIFY_FILENAME: &str = "codex-notify.mjs";

/// The hook events we install. `UserPromptSubmit` is the turn-START edge the
/// old notify path never had; `Stop` is turn end. The shim maps them to
/// running / idle.
const HOOK_EVENTS: &[&str] = &["UserPromptSubmit", "Stop"];

/// The default launch command — THE one definition for this agent. The spawn
/// below and the settings UI (via [`crate::default_command_display`]) both
/// derive from these two consts.
pub(crate) const PROGRAM: &str = "codex";
pub(crate) const DEFAULT_ARGS: &[&str] =
    &["--sandbox", "workspace-write", "--ask-for-approval", "on-request"];
const NO_DAEMON: &str = "--no-daemon";

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

    pub fn materialize_shim(&self) -> io::Result<PathBuf> {
        crate::install_remote_tab_helper(&self.state_dir)?;
        let path = self.shim_path();
        crate::install_shim(&path, SHIM)?;
        Ok(path)
    }

    /// Where codex reads its global config: `$CODEX_HOME`, else `~/.codex`.
    /// Both `config.toml` and `hooks.json` live here. Overridable for tests via
    /// `HARNESS_CODEX_HOME`.
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

    /// The shell command codex runs for the hook — `node "<shim>"`. Debug
    /// formatting quotes and escapes the path, so a `$HOME` with spaces stays
    /// one argument.
    fn hook_command(shim: &Path) -> String {
        format!("node {:?}", shim.display().to_string())
    }

    fn install_hook(&self, shim: &Path) -> io::Result<()> {
        install_status_hooks(&self.codex_home(), &shim.display().to_string(), &Self::hook_command(shim))
    }

    /// Best-effort removal of the pre-hooks `notify` line from `config.toml`,
    /// so the old shim and the new hook don't both write `status.json`. Purely
    /// a migration cleanup: parse-guarded, removal-only, atomic, and it writes
    /// only when it actually changed something — and it never fails the spawn
    /// (an unparseable or unwritable config is left exactly as it was).
    fn remove_legacy_notify(&self) {
        let path = self.codex_home().join("config.toml");
        let Ok(existing) = fs::read_to_string(&path) else {
            return;
        };
        let Ok(mut doc) = existing.parse::<toml_edit::DocumentMut>() else {
            return;
        };
        let marker = self
            .state_dir
            .join("shims")
            .join(LEGACY_NOTIFY_FILENAME)
            .display()
            .to_string();

        let mut changed = false;
        // The root `notify` we used to write, plus any stray copies the old
        // line-append bug left inside nested tables.
        if doc
            .get("notify")
            .map(|v| v.to_string().contains(&marker))
            .unwrap_or(false)
        {
            doc.remove("notify");
            changed = true;
        }
        changed |= remove_stray_notify(doc.as_table_mut(), &marker);

        if changed {
            let tmp = path.with_extension("toml.tmp");
            if fs::write(&tmp, doc.to_string()).is_ok() {
                let _ = fs::rename(&tmp, &path);
            }
        }
    }
}

/// Remove `notify` keys referencing `marker` from every NESTED table (not the
/// root — the caller handles that). Returns whether anything was removed.
/// Cleans up configs the pre-`toml_edit` line-append bug damaged.
fn remove_stray_notify(table: &mut toml_edit::Table, marker: &str) -> bool {
    let mut changed = false;
    let keys: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for key in keys {
        if let Some(child) = table.get_mut(&key).and_then(|i| i.as_table_mut()) {
            if child
                .get("notify")
                .map(|v| v.to_string().contains(marker))
                .unwrap_or(false)
            {
                child.remove("notify");
                changed = true;
            }
            changed |= remove_stray_notify(child, marker);
        }
    }
    changed
}

/// Merge status hooks without changing the positions Codex uses to track trust.
pub(crate) fn install_status_hooks(dir: &Path, marker: &str, command: &str) -> io::Result<()> {
    crate::update_agent_hooks(dir, |root| {
        for event in HOOK_EVENTS {
            ensure_hook(root, event, marker, command);
        }
        Ok(())
    })
}

/// Update our first existing command at its current index and remove duplicates.
fn ensure_hook(root: &mut Value, event: &str, marker: &str, command: &str) {
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
    let mut found = false;
    arr.retain_mut(|group| {
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let was_empty = handlers.is_empty();
        handlers.retain_mut(|handler| {
            let belongs_to_us = handler["command"].as_str().is_some_and(|cmd| cmd.contains(marker));
            if !belongs_to_us {
                return true;
            }
            if found {
                return false;
            }
            found = true;
            handler["command"] = json!(command);
            true
        });
        was_empty || !handlers.is_empty()
    });
    if !found {
        arr.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
    }
}

impl AgentRuntime for CodexRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn fidelity(&self) -> WaitFidelity {
        // Trusted turn hooks report running and idle. PermissionRequest runs
        // before approval routing and does not prove that a user prompt opened,
        // so the adapter does not send a waiting notification from it.
        WaitFidelity::BestEffort
    }

    fn signal_kind(&self) -> SignalKind {
        // The hook writes the shared `.harness/status.json`, same as pi and
        // Claude Code — consumed by StatusFileSource, not a codex-specific path.
        SignalKind::StatusFile
    }

    fn ignored_paths(&self) -> &'static [&'static str] {
        &[".harness/", ".codex/"]
    }

    fn on_spawn(&self, _worktree_root: &Path) -> io::Result<()> {
        let shim = self.materialize_shim()?;
        // Codex reads ONE global config dir: $CODEX_HOME (default ~/.codex).
        // The status hook goes in hooks.json there; the migration below strips
        // the `notify` line the old mechanism left in config.toml.
        self.install_hook(&shim)?;
        self.remove_legacy_notify();
        Ok(())
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        Some(AgentCommand {
            program: resolve_on_path(PROGRAM)?,
            args: DEFAULT_ARGS.iter().map(|s| s.to_string()).collect(),
            // The global status hook only writes when it inherits this marker
            // from a harness-spawned Codex process.
            env: vec![("HARNESS_MANAGED".to_string(), "1".to_string())],
            label: PROGRAM.to_string(),
        })
    }

    fn prepare_launch(&self, mut command: AgentCommand) -> AgentCommand {
        // A shared Codex server runs hooks with its own environment, which
        // lacks this terminal's HARNESS_MANAGED and HARNESS_REMOTE_SESSION.
        // Keep the server local to the CLI so both reach the hooks. Older CLI
        // releases reject the flag. Shell-based command overrides own their
        // arguments and are passed through by the caller.
        if command.program.file_name() == Some(PROGRAM.as_ref())
            && !command.args.iter().any(|arg| arg == NO_DAEMON)
            && Command::new(&command.program)
                .args([NO_DAEMON, "--version"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        {
            command.args.push(NO_DAEMON.into());
        }
        command
    }
}
