//! The shared `.harness/status.json` signal source (§4.5).
//!
//! Two adapters normalize through a file rather than an in-process stream: pi
//! (via `harness-status.ts`) and Claude Code (via a `Notification` hook shim,
//! A3). Both write the *same* three-state file, so one watcher serves both and
//! `status-watch` consumes them unchanged.
//!
//! This owns exactly one [`StatusWatcher`] for the whole app — not one per
//! worktree — and pumps its events onto the [`Bus`] as [`AgentSignal`]s.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use status_watch::{AgentState, StatusWatcher, WatchError};

use crate::{AgentKind, AgentSignal, Bus, WaitReason};

/// Watches every `StatusFile`-signalled worktree and republishes onto the bus.
pub struct StatusFileSource {
    watcher: StatusWatcher,
    /// Which agent owns each watched root, so the signal carries the right
    /// kind (pi and Claude Code share this file format).
    kinds: Arc<Mutex<HashMap<PathBuf, AgentKind>>>,
    bus: Bus,
}

impl StatusFileSource {
    /// Start the watcher and the pump thread that feeds `bus`.
    pub fn new(bus: Bus) -> Result<Self, WatchError> {
        let (watcher, rx) = StatusWatcher::new()?;
        let kinds: Arc<Mutex<HashMap<PathBuf, AgentKind>>> = Arc::new(Mutex::new(HashMap::new()));

        let pump_kinds = Arc::clone(&kinds);
        let pump_bus = bus.clone();
        std::thread::Builder::new()
            .name("status-file-source".into())
            .spawn(move || {
                // Ends when the watcher is dropped and the channel closes.
                for event in rx {
                    // The file's own claim beats the registered kind: every
                    // agent writes the same status.json, and the registered
                    // kind is only the worktree's DEFAULT — a claude launched
                    // in a pi worktree must not be announced as pi.
                    let kind = match event.status.agent.as_deref() {
                        Some(agent) => AgentKind::from(agent),
                        None => pump_kinds
                            .lock()
                            .unwrap()
                            .get(&event.worktree_root)
                            .cloned()
                            .unwrap_or_default(),
                    };
                    pump_bus.publish(AgentSignal {
                        worktree_root: event.worktree_root,
                        tab_id: event.tab_id,
                        kind,
                        state: event.status.state,
                        reason: reason_for(event.status.state),
                        session_file: event.status.session_file.map(PathBuf::from),
                        tool: event.status.tool,
                    });
                }
            })
            .map_err(WatchError::Io)?;

        Ok(StatusFileSource {
            watcher,
            kinds,
            bus,
        })
    }

    /// Start watching a worktree, attributing its signals to `kind`.
    pub fn watch(&mut self, worktree_root: &Path, kind: AgentKind) -> Result<(), WatchError> {
        self.kinds
            .lock()
            .unwrap()
            .insert(worktree_root.to_path_buf(), kind);
        self.watcher.watch(worktree_root)
    }

    pub fn unwatch(&mut self, worktree_root: &Path) -> Result<(), WatchError> {
        self.kinds.lock().unwrap().remove(worktree_root);
        self.watcher.unwatch(worktree_root)
    }

    pub fn bus(&self) -> &Bus {
        &self.bus
    }
}

/// Map a file-based state to a wait reason.
///
/// The status file carries no reason of its own — that is exactly the fidelity
/// ceiling R8 describes. `waiting` is only ever written while a dialog is open,
/// so attributing it to a permission prompt is accurate for pi and for the
/// Claude Code hook (whose `permission_prompt` matcher writes the same state).
fn reason_for(state: AgentState) -> WaitReason {
    match state {
        AgentState::Waiting => WaitReason::PermissionPrompt,
        AgentState::Idle => WaitReason::TurnComplete,
        AgentState::Running | AgentState::Unknown => WaitReason::None,
    }
}
