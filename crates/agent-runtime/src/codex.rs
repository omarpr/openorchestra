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
//! Fidelity stays [`WaitFidelity::BestEffort`]: we now know running vs idle
//! reliably, but codex (running with `--ask-for-approval never`) exposes no
//! out-of-band "blocked on you" event, so `should_notify` still refuses to
//! raise a "needs input" notification — dressing turn state up as "input
//! needed" is exactly the lie R11 exists to prevent.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
// No `--sandbox` flag: the workspace-write sandbox blocked too much (network,
// paths outside the worktree) and, paired with `--ask-for-approval never`,
// those blocks became silent hard failures instead of prompts. Dropping it
// lets Codex fall back to its own default sandbox from `~/.codex/config.toml`,
// so policy lives where the user controls it. Approvals stay off — the harness
// runs Codex unattended.
//
// `--dangerously-bypass-hook-trust` runs the status hook we install without a
// per-launch trust prompt. It only affects harness-spawned codex, and the
// hooks it trusts are the user's own (~/.codex/hooks.json) — but codex would
// otherwise block the first launch waiting for a trust decision the harness
// cannot answer.
pub(crate) const DEFAULT_ARGS: &[&str] =
    &["--ask-for-approval", "never", "--dangerously-bypass-hook-trust"];

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

    /// Install our status hook into `~/.codex/hooks.json` under every event in
    /// [`HOOK_EVENTS`], merging with whatever hooks the user already has.
    ///
    /// hooks.json shape (Claude-Code compatible):
    /// `{ "hooks": { "<Event>": [ { "hooks": [ {"type":"command","command":".."} ] } ] } }`
    ///
    /// Idempotent and non-clobbering: [`ensure_hook`] drops any prior group
    /// that referenced *our* shim before re-adding one, so re-runs don't
    /// accumulate and the user's own hooks (a different command) are untouched.
    fn install_hook(&self, shim: &Path) -> io::Result<()> {
        let dir = self.codex_home();
        let path = dir.join("hooks.json");

        let existing = fs::read_to_string(&path).unwrap_or_default();
        let mut root: Value = if existing.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&existing).map_err(|e| {
                // A hooks file we cannot parse is one we must not rewrite. The
                // caller only logs this, so codex still launches — just without
                // the status hook.
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

        let marker = shim.display().to_string();
        let command = Self::hook_command(shim);
        for event in HOOK_EVENTS {
            ensure_hook(&mut root, event, &marker, &command);
        }

        fs::create_dir_all(&dir)?;
        let tmp = dir.join(".hooks.json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(&root)? + "\n")?;
        fs::rename(&tmp, &path)
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

/// Ensure `root["hooks"][event]` contains exactly one hook group that runs
/// `command`, keyed for idempotency by `marker` (the shim path). Existing
/// groups that mention the marker are replaced; foreign groups are preserved.
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
    // Drop any earlier copy of OUR hook (matched by the shim path) so re-runs
    // stay flat; leave the user's own hooks alone.
    arr.retain(|group| !group.to_string().contains(marker));
    arr.push(json!({ "hooks": [ { "type": "command", "command": command } ] }));
}

impl AgentRuntime for CodexRuntime {
    fn kind(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn fidelity(&self) -> WaitFidelity {
        // Running/idle is now reliable (UserPromptSubmit / Stop hooks), but
        // codex exposes no "blocked on you" event — so it stays BestEffort and
        // never claims "input needed" (R11).
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
            // The notify hook lives in the user's GLOBAL codex config; this
            // marker is what scopes it — the shim exits without writing
            // unless it inherits this from a harness-spawned codex.
            env: vec![("HARNESS_MANAGED".to_string(), "1".to_string())],
            label: PROGRAM.to_string(),
        })
    }
}
