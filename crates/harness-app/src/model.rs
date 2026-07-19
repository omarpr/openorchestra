//! Agent status for the sidebar pill. In Spike 1 the whole sidebar was mock;
//! Spike 4 replaced the worktree/diff mocks with the real registry + git
//! (`data.rs`). The agent *status* itself still comes from Spike 2's
//! `status-watch` pipeline, which is not yet wired into the app — the pill is a
//! static placeholder until then.

// Running/Waiting are unused until the status-watch pipeline is wired in.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    Running,
    Waiting,
    Idle,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::Running => "Running",
            AgentStatus::Waiting => "Waiting",
            AgentStatus::Idle => "Idle",
        }
    }
}
