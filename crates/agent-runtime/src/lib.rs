//! agent-runtime — the agent-agnostic layer (architecture §4.5–4.6, D12).
//!
//! **The substrate is the terminal.** Running an agent means spawning that
//! vendor's own CLI in the worktree's pty, exactly as the harness already does
//! for pi. Every agent works that way; nothing here is required for an agent to
//! be usable.
//!
//! What this crate adds on top is the *optional* enhancement: an
//! [`AgentRuntime`] per vendor that knows (a) which command to run plus any
//! per-agent config to drop in, and (b) — where the agent exposes a signal
//! surface — how that agent's native "needs input" event normalizes into the
//! same three states the sidebar already consumes.
//!
//! Signal fidelity is deliberately heterogeneous and reported honestly via
//! [`WaitFidelity`] (R11): an agent with no signal surface still runs fully in
//! its terminal, it just shows a coarser status and raises no notification.
//!
//! UI-free like `worktrees` / `forge` / `status-watch`, so the same layer can
//! drive batch evals with no window.

mod bus;
mod claude_code;
mod codex;
mod configurable;
mod grok;
mod kimi;
mod opencode;
mod pi;
pub mod sse;
mod status_file;
mod task_context;

pub use bus::Bus;
pub use claude_code::ClaudeCodeRuntime;
pub use codex::CodexRuntime;
pub use configurable::{
    harness_status_shim_command, harness_status_shim_path, materialize_harness_status_shim,
    ConfigurableRuntime, HookInstall, SignalProfile,
};
pub use grok::GrokRuntime;
pub use kimi::KimiRuntime;
pub use opencode::OpenCodeRuntime;
pub use pi::PiRuntime;
pub use status_file::StatusFileSource;
pub use task_context::{
    apply_task_context, shell_command_with_task_context, task_context_prompt, HARNESS_TASK_CONTEXT,
};

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use status_watch::{status_path, AgentState, ToolCall};
pub use worktrees::AgentKind;

/// Why an agent is waiting, when it tells us (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    /// Blocked on an approval / permission prompt.
    PermissionPrompt,
    /// Blocked at an idle prompt, waiting for the next instruction.
    IdlePrompt,
    /// Not blocked — the turn simply finished.
    TurnComplete,
    None,
}

/// How much to trust an agent's "waiting" signal (R11). The UI reflects this
/// rather than pretending every agent reports the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitFidelity {
    /// A real, out-of-band "I am blocked on you" event.
    Exact,
    /// Only coarser evidence — e.g. turn-complete, but no approval signal.
    BestEffort,
    /// No signal surface at all; the terminal is the whole story.
    TerminalOnly,
}

/// How an adapter's enhancement arrives, if at all (§4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// Writes the shared `.harness/status.json` (pi, and Claude Code via a hook
    /// shim). Consumed by [`StatusFileSource`], not by the adapter itself.
    StatusFile,
    /// Server-sent events from the agent's local HTTP server. No built-in
    /// adapter declares it now; OpenCode 1.x's SSE survives only as that
    /// adapter's fallback stream.
    Sse,
    /// A `notify` program the agent invokes (Codex).
    Notify,
    /// Agent Client Protocol over JSON-RPC (Kimi via `kimi acp`).
    Acp,
    /// Terminal-only — a supported first-class case, not a failure.
    None,
}

/// One normalized status update for a worktree, from any adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSignal {
    pub worktree_root: PathBuf,
    /// The terminal that produced the update, when the launch is tracked.
    pub tab_id: Option<u64>,
    pub kind: AgentKind,
    pub state: AgentState,
    pub reason: WaitReason,
    /// Path to the agent's session transcript, when the adapter exposes one —
    /// the harvesting hook for §6.4 / M7.
    pub session_file: Option<PathBuf>,
    /// The tool the agent is about to run, when the adapter reports one.
    pub tool: Option<ToolCall>,
}

impl AgentSignal {
    /// Whether this transition should raise a native "needs input"
    /// notification (§4.6). Only an exact signal may claim the user is
    /// actually blocked — a best-effort adapter reporting turn-complete must
    /// not be dressed up as "input needed".
    pub fn should_notify(&self, fidelity: WaitFidelity) -> bool {
        self.state == AgentState::Waiting && fidelity == WaitFidelity::Exact
    }

    /// Whether this signal warrants a "finished — your turn" notification.
    ///
    /// Distinct from [`should_notify`] and deliberately more permissive on
    /// fidelity: R11 forbids dressing a best-effort signal up as "input
    /// needed", but "the turn ended" is *exactly* what a best-effort adapter
    /// (Codex's notify, pi's status file) actually knows. Only the edge
    /// counts — Running → Idle — so an agent that idles at startup stays
    /// quiet.
    pub fn should_notify_finished(
        &self,
        previous: Option<AgentState>,
        fidelity: WaitFidelity,
    ) -> bool {
        previous == Some(AgentState::Running)
            && self.state == AgentState::Idle
            && fidelity != WaitFidelity::TerminalOnly
    }
}

/// The fidelity an adapter of this kind declares, without building one.
///
/// The signal path needs this when a status file changes for a worktree
/// whose agent tab was never opened this session: the per-worktree fidelity
/// cache only fills on spawn, and defaulting to `TerminalOnly` silently
/// swallowed every notification after an app restart.
pub fn fidelity_for(kind: &AgentKind) -> WaitFidelity {
    match kind {
        AgentKind::Pi => WaitFidelity::BestEffort,
        AgentKind::OpenCode => WaitFidelity::Exact,
        AgentKind::Codex => WaitFidelity::BestEffort,
        AgentKind::ClaudeCode | AgentKind::Grok => WaitFidelity::Exact,
        AgentKind::Kimi | AgentKind::Other(_) => WaitFidelity::TerminalOnly,
    }
}

/// The command to run in the worktree's pty.
///
/// Deliberately data, not a `std::process::Command`: the terminal pane builds
/// its own pty spawn from this, and tests can assert on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentCommand {
    /// Absolute path to the resolved binary.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Short label for the tab / status bar (e.g. `"pi"`).
    pub label: String,
}

/// One vendor's adapter (§4.5).
///
/// The harness bundles, redistributes, and links none of these agents — each is
/// the user's own installed CLI resolved from `$PATH` and run as a separate
/// subprocess, so there is no license coupling and no ToS concern (R12).
pub trait AgentRuntime: Send + Sync {
    fn kind(&self) -> AgentKind;

    /// How much to trust this adapter's `Waiting` (R11).
    fn fidelity(&self) -> WaitFidelity;

    /// How the optional enhancement arrives, if at all.
    fn signal_kind(&self) -> SignalKind;

    /// Paths this adapter writes into the worktree, for the caller to add to
    /// the repo's local git excludes.
    ///
    /// The harness runs inside the user's own repositories, so anything it
    /// drops there would otherwise show up as uncommitted changes in their
    /// project — and in Claude Code's case carry a machine-specific absolute
    /// path that must never be committed.
    fn ignored_paths(&self) -> &'static [&'static str] {
        &[]
    }

    /// Prepare the worktree before spawning: drop in whatever per-agent config
    /// enables the signal surface (a pi extension, a `.claude/settings.json`
    /// hook, a `notify` entry). Must be idempotent — it runs on every spawn.
    ///
    /// A failure here is never fatal: the agent still runs in its terminal, it
    /// just loses the enhancement. Callers should log and continue.
    fn on_spawn(&self, worktree_root: &Path) -> std::io::Result<()>;

    /// The CLI to run in the pty, or `None` when the binary is not installed —
    /// in which case the caller falls back to a plain shell rather than
    /// failing, since a terminal is still useful.
    fn command(&self, worktree_root: &Path) -> Option<AgentCommand>;

    /// Apply adapter-specific launch wiring after the app has resolved overrides and resume args.
    fn prepare_launch(&self, command: AgentCommand) -> AgentCommand {
        command
    }

    /// A stream of signals this adapter produces itself.
    ///
    /// `None` for adapters whose signal arrives through a shared source instead
    /// (anything with [`SignalKind::StatusFile`] — see [`StatusFileSource`]),
    /// and for terminal-only agents.
    fn subscribe(&self) -> Option<crossbeam_channel::Receiver<AgentSignal>> {
        None
    }
}

/// The binary name and default args a kind launches, for any caller that
/// touches an agent CLI outside an adapter — presence checks, sidecar
/// spawns, the settings display.
///
/// This does NOT define anything: each agent's `PROGRAM` / `DEFAULT_ARGS`
/// consts in its own module are the single definition, shared with that
/// adapter's `command()`.
pub fn default_invocation(kind: &AgentKind) -> (&str, &'static [&'static str]) {
    match kind {
        AgentKind::Pi => (pi::PROGRAM, pi::DEFAULT_ARGS),
        AgentKind::OpenCode => (opencode::PROGRAM, opencode::DEFAULT_ARGS),
        AgentKind::ClaudeCode => (claude_code::PROGRAM, claude_code::DEFAULT_ARGS),
        AgentKind::Codex => (codex::PROGRAM, codex::DEFAULT_ARGS),
        AgentKind::Kimi => (kimi::PROGRAM, kimi::DEFAULT_ARGS),
        AgentKind::Grok => (grok::PROGRAM, grok::DEFAULT_ARGS),
        other => (other.as_str(), &[]),
    }
}

/// `args` rewritten to continue the agent's most recent conversation **in the
/// directory it is launched in**, for a restart after the agent went missing
/// (computer restart, agent quit). `None` when the kind has no
/// directory-scoped continue: a resume that could land in another folder's
/// conversation is worse than a fresh start, so those kinds restart fresh.
///
/// - Claude Code `--continue`: "most recent conversation in the current directory".
/// - pi `--continue`: "most recent session for the current directory".
/// - Grok `--continue`: "most recent session for the current working directory".
/// - Codex `resume --last`: `resume` filters by cwd unless `--all` is given.
/// - OpenCode `--continue` is "the last session" of a *project*, which sibling
///   worktrees of one repo can share, so it is not used.
pub fn continue_args(kind: &AgentKind, args: &[String]) -> Option<Vec<String>> {
    let owned = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match kind {
        AgentKind::ClaudeCode | AgentKind::Pi | AgentKind::Grok => {
            let mut out = args.to_vec();
            out.push("--continue".into());
            Some(out)
        }
        AgentKind::Codex => {
            let mut out = owned(&["resume", "--last"]);
            out.extend(args.iter().cloned());
            Some(out)
        }
        _ => None,
    }
}

/// Whether `dir` has a saved conversation for [`continue_args`] to pick up.
/// `Some(false)` lets a restart start fresh straight away instead of
/// launching a continue that fails ("No conversation found to continue")
/// and leaves the agent stopped. `None` when the kind's storage is not known
/// here: the caller tries continuing and falls back on the next click.
///
/// Claude Code keeps conversations as `*.jsonl` under
/// `$CLAUDE_CONFIG_DIR` (default `~/.claude`) `/projects/<dir>`, with every
/// non-alphanumeric character of the absolute path turned into `-`.
pub fn has_conversation(kind: &AgentKind, dir: &Path) -> Option<bool> {
    match kind {
        AgentKind::ClaudeCode => {
            let config = std::env::var_os("CLAUDE_CONFIG_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".claude")))?;
            let projects = config.join("projects").join(claude_project_slug(dir));
            let found = std::fs::read_dir(&projects)
                .map(|entries| {
                    entries
                        .flatten()
                        .any(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                })
                .unwrap_or(false);
            Some(found)
        }
        _ => None,
    }
}

/// Claude Code's folder name for a working directory's conversations.
pub fn claude_project_slug(dir: &Path) -> String {
    dir.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn install_remote_tab_helper(state_dir: &Path) -> std::io::Result<()> {
    const HELPER: &str = include_str!("../shims/remote-tab.mjs");
    install_shim(&state_dir.join("shims/remote-tab.mjs"), HELPER)
}

fn install_shim(target: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write as _;

    if std::fs::read_to_string(target).is_ok_and(|text| text == contents) {
        return Ok(());
    }
    let dir = target.parent().expect("shim path has a parent");
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    persist_shim(tmp, target, contents)
}

/// Update a user hooks file atomically, keeping concurrent launches from
/// replacing each other's registrations.
fn update_agent_hooks(
    dir: &Path,
    update: impl FnOnce(&mut serde_json::Value) -> std::io::Result<()>,
) -> std::io::Result<()> {
    use std::fs;
    use std::io;

    fs::create_dir_all(dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(".openorchestra-hooks.lock"))?;
    lock.lock()?;
    let path = dir.join("hooks.json");
    let existing = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&existing).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} does not parse: {error}", path.display()),
            )
        })?
    };
    if !root.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "hooks.json is not an object",
        ));
    }
    let previous = root.clone();
    update(&mut root)?;
    if root != previous {
        install_shim(&path, &(serde_json::to_string_pretty(&root)? + "\n"))?;
    }
    Ok(())
}

/// The default launch command for a kind, as one displayable line — e.g.
/// `"claude --permission-mode auto"`.
///
/// The settings UI shows and prefills this string so what the user sees is
/// exactly what an un-overridden spawn runs.
pub fn default_command_display(kind: &AgentKind) -> String {
    let (program, args) = default_invocation(kind);
    std::iter::once(program)
        .chain(args.iter().copied())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Find an executable named `program` on `$PATH`.
///
/// Deliberately does not consult `$SHELL` or a bundled copy: §4.5/R12 rests on
/// this being *the user's own installed, authenticated CLI*.
pub fn resolve_on_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let names = os_compat::Os::CURRENT.executable_names(program);
    std::env::split_paths(&path_var)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Pick the adapter for a worktree's recorded [`AgentKind`].
///
/// Unknown kinds (a registry written by a newer build) fall back to a
/// terminal-only adapter that runs the kind's own name as the command — the
/// honest behavior: we can still launch it, we just have no signal.
///
/// Custom agents with a configured [`SignalProfile`] should use
/// [`runtime_for_with_signal`] so hooks and fidelity apply.
pub fn runtime_for(
    kind: &AgentKind,
    worktree_root: &Path,
    state_dir: &Path,
) -> Box<dyn AgentRuntime> {
    runtime_for_with_signal(kind, worktree_root, state_dir, None)
}

/// Like [`runtime_for`], but applies `signal` to [`AgentKind::Other`].
///
/// Built-in kinds ignore `signal` — they keep their dedicated adapters.
/// `Other` with `Some(profile)` gets a [`ConfigurableRuntime`]; without, the
/// terminal-only baseline.
pub fn runtime_for_with_signal(
    kind: &AgentKind,
    worktree_root: &Path,
    state_dir: &Path,
    signal: Option<SignalProfile>,
) -> Box<dyn AgentRuntime> {
    match kind {
        AgentKind::Pi => Box::new(PiRuntime::new(state_dir)),
        AgentKind::OpenCode => {
            Box::new(OpenCodeRuntime::new(worktree_root).with_state_dir(state_dir))
        }
        AgentKind::ClaudeCode => Box::new(ClaudeCodeRuntime::new(state_dir)),
        AgentKind::Codex => Box::new(CodexRuntime::new(state_dir)),
        AgentKind::Kimi => Box::new(KimiRuntime),
        AgentKind::Grok => Box::new(GrokRuntime::new(state_dir)),
        other => match signal {
            Some(profile) => Box::new(ConfigurableRuntime::new(other.clone(), state_dir, profile)),
            None => Box::new(TerminalOnlyRuntime::new(other.clone())),
        },
    }
}

/// The baseline adapter: run the agent's CLI, offer no signal.
///
/// This is what every agent gets before its dedicated adapter exists, and what
/// agents with no signal surface (Kimi's TUI) get permanently.
pub struct TerminalOnlyRuntime {
    kind: AgentKind,
}

impl TerminalOnlyRuntime {
    pub fn new(kind: AgentKind) -> Self {
        TerminalOnlyRuntime { kind }
    }
}

impl AgentRuntime for TerminalOnlyRuntime {
    fn kind(&self) -> AgentKind {
        self.kind.clone()
    }

    fn fidelity(&self) -> WaitFidelity {
        WaitFidelity::TerminalOnly
    }

    fn signal_kind(&self) -> SignalKind {
        SignalKind::None
    }

    fn on_spawn(&self, _worktree_root: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn command(&self, _worktree_root: &Path) -> Option<AgentCommand> {
        // Same single source as the dedicated adapters: known kinds get their
        // module's PROGRAM / DEFAULT_ARGS (claude-code's binary is `claude`),
        // unknown kinds run their own wire string with no args.
        let (program, args) = default_invocation(&self.kind);
        Some(AgentCommand {
            program: resolve_on_path(program)?,
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Vec::new(),
            label: program.to_string(),
        })
    }
}

/// Re-export of the OpenCode 1.x event classifier (the SSE fallback), so the
/// mapping table in §4.5 can be asserted directly without exposing the whole
/// module.
pub use opencode::classify as opencode_classify;

/// Whether `haystack` (a serialized hook entry or command) mentions `marker`
/// (a shim path). JSON, TOML and `{:?}` all escape a Windows path's `\` as
/// `\\`, so a plain `contains` never matched there and every spawn appended
/// another copy of our hooks. Unix paths have no backslashes: unchanged.
pub(crate) fn mentions(haystack: &str, marker: &str) -> bool {
    haystack.contains(marker)
        || (marker.contains('\\') && haystack.contains(&marker.replace('\\', "\\\\")))
}

/// The hook command that runs a status shim with node.
///
/// On Windows the agents run hook commands through Git Bash, where an
/// unquoted `C:\Users\…\shim.mjs` loses its backslashes (`C:Users…`) and node
/// finds nothing: there it is `node "C:/Users/…/shim.mjs"`, which bash and
/// cmd both read as one path. Elsewhere it is unchanged.
pub(crate) fn node_command(shim: &Path) -> String {
    use os_compat::Os;
    match Os::CURRENT {
        Os::Windows => format!("node \"{}\"", Os::Windows.shell_path(shim)),
        Os::Mac | Os::Linux => format!("node {}", shim.display()),
    }
}

/// [`mentions`] for a shim path (`marker`, as `display()` writes it), in
/// either spelling [`node_command`] has used, so hooks written before the
/// forward-slash form are replaced rather than kept beside it.
pub(crate) fn mentions_shim(haystack: &str, marker: &str) -> bool {
    mentions(haystack, marker) || (marker.contains('\\') && haystack.contains(&marker.replace('\\', "/")))
}

/// `tmp.persist(target)`, tolerant of concurrent launches installing the same
/// shim. On Windows, replacing a file another thread is replacing at that
/// instant fails with Access Denied (Unix `rename` just wins or loses): retry
/// briefly, and treat "the target already holds exactly our contents" as done.
pub(crate) fn persist_shim(
    mut tmp: tempfile::NamedTempFile,
    target: &Path,
    contents: &str,
) -> std::io::Result<()> {
    for _ in 0..50 {
        match tmp.persist(target) {
            Ok(_) => return Ok(()),
            Err(e) => {
                if std::fs::read_to_string(target).is_ok_and(|t| t == contents) {
                    return Ok(());
                }
                if e.error.kind() != std::io::ErrorKind::PermissionDenied {
                    return Err(e.error);
                }
                tmp = e.file;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
    tmp.persist(target).map(|_| ()).map_err(|e| e.error)
}
