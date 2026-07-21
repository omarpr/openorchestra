//! Deriving the agent's normalized state from the RPC event stream.
//!
//! §4.2 assumed an `agent_settled` event would make this trivial. It does not
//! exist at pi 0.73.1 (see [`crate::protocol`]), so the "is it really done?"
//! question still has to be answered by the client — but with far better
//! inputs than the Phase 1 extension had, which is what actually retires R8:
//!
//! | R8 gap | Phase 1 (extension) | Phase 2 (RPC) |
//! |---|---|---|
//! | auto-retry blips idle | `agent_end`/`agent_start` re-fire with no flag, so a 750 ms debounce guesses | `auto_retry_start` says so **explicitly** — no guessing, no debounce, no blip however long the backoff |
//! | dialogs invisible | only those opened through the shared `uiContext` | `extension_ui_request` reports every one, with an id |
//!
//! So the debounce is gone. State transitions are now derived from facts.

use crate::protocol::{ExtensionUiRequest, RpcEvent, RpcRecord};

/// The normalized agent state — the same three the sidebar consumes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Waiting,
    Idle,
}

/// Folds the RPC event stream into [`AgentState`].
#[derive(Debug, Default)]
pub struct SettleTracker {
    running: bool,
    /// Number of blocking dialogs currently open. A counter, not a flag:
    /// nested dialogs must not drop back to Running when the inner one closes.
    open_dialogs: usize,
    /// True between `auto_retry_start` and `auto_retry_end` — the agent is
    /// still working even though `agent_end` already fired.
    retrying: bool,
}

impl SettleTracker {
    pub fn new() -> Self {
        SettleTracker::default()
    }

    /// Current state.
    pub fn state(&self) -> AgentState {
        if self.open_dialogs > 0 {
            AgentState::Waiting
        } else if self.running || self.retrying {
            AgentState::Running
        } else {
            AgentState::Idle
        }
    }

    /// Feed one record; returns the new state if it changed.
    pub fn observe(&mut self, record: &RpcRecord) -> Option<AgentState> {
        let before = self.state();
        match record {
            RpcRecord::Event(RpcEvent::AgentStart) => {
                self.running = true;
                self.retrying = false;
            }
            RpcRecord::Event(RpcEvent::AgentEnd { .. }) => {
                self.running = false;
            }
            // An imminent retry means the agent is *not* done, no matter that
            // agent_end just fired. This is the exact signal Phase 1 lacked.
            RpcRecord::Event(RpcEvent::AutoRetryStart { .. }) => {
                self.retrying = true;
            }
            RpcRecord::Event(RpcEvent::AutoRetryEnd { .. }) => {
                self.retrying = false;
            }
            RpcRecord::ExtensionUiRequest(req) if req.is_blocking() => {
                self.open_dialogs += 1;
            }
            _ => {}
        }
        let after = self.state();
        (after != before).then_some(after)
    }

    /// Record that a blocking dialog was answered.
    ///
    /// Separate from [`observe`] because the *reply* is something the harness
    /// sends, not something pi emits — there is no "dialog closed" event.
    ///
    /// [`observe`]: SettleTracker::observe
    pub fn dialog_answered(&mut self) -> Option<AgentState> {
        let before = self.state();
        self.open_dialogs = self.open_dialogs.saturating_sub(1);
        let after = self.state();
        (after != before).then_some(after)
    }

    /// Whether a blocking dialog is currently open.
    pub fn is_blocked(&self) -> bool {
        self.open_dialogs > 0
    }
}

/// Whether a record is a blocking dialog the UI must answer.
pub fn blocking_request(record: &RpcRecord) -> Option<&ExtensionUiRequest> {
    match record {
        RpcRecord::ExtensionUiRequest(req) if req.is_blocking() => Some(req),
        _ => None,
    }
}
