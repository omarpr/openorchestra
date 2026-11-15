//! Restarting a task agent that went missing (computer restart, agent quit).
//!
//! A task pane records which agent it runs (`WorktreePanes::agent`). A
//! background check (see `construct.rs`) compares that against what tmux is
//! actually running and fills `missing_agents`; the sidebar shows those as
//! STOPPED, and clicking one calls [`HarnessWorkspace::restart_task_agent`].
//! Nothing here starts an agent unless the user asks.

use super::*;

/// What the coordinator is told after a restart, so completions that arrived
/// while it was gone (queued A2A lines live only in memory) are not lost.
const COORDINATOR_RESTART_NUDGE: &str = "[openOrchestra] You were restarted. Check the status of every \
delegation you have open (./a2a/status.sh <a2a_task_id>) and report anything that finished while you \
were away.";

impl HarnessWorkspace {
    /// Pane keys of active tasks whose pane is meant to run an agent. Ended or
    /// ending tasks are skipped: their agents are being torn down on purpose.
    pub(crate) fn task_panes_expecting_agents(&self) -> Vec<String> {
        let mut keys = Vec::new();
        for task in self.task_store.active() {
            if self.ending.contains_key(&task.id) {
                continue;
            }
            let mut candidates: Vec<String> = task
                .members
                .iter()
                .map(|m| a2a_broker::task_member_pane_key(&task.id, &m.worktree_id))
                .collect();
            if task.members.len() > 1 {
                candidates.push(a2a_broker::coordinator_pane_key(&task.id));
            }
            keys.extend(candidates.into_iter().filter(|k| {
                self.panes_state.worktrees.get(k).is_some_and(|w| w.agent.is_some())
            }));
        }
        keys
    }

    pub(crate) fn set_missing_agents(
        &mut self,
        missing: std::collections::BTreeSet<String>,
        cx: &mut Context<Self>,
    ) {
        // An agent that is running again no longer needs the fresh-start fallback.
        self.restart_tried.retain(|k| missing.contains(k));
        if missing != self.missing_agents {
            self.missing_agents = missing;
            cx.notify();
        }
    }

    pub(crate) fn agent_missing(&self, task_id: &str, member_id: Option<&str>) -> bool {
        self.missing_agents.contains(&restart_pane_key(task_id, member_id))
    }

    /// Whether the next Restart of this pane starts fresh, because continuing
    /// the conversation was already tried and the agent is still not running.
    pub(crate) fn restart_is_fresh(&self, task_id: &str, member_id: Option<&str>) -> bool {
        self.restart_tried.contains(&restart_pane_key(task_id, member_id))
    }

    /// Bring a missing task agent back: open its pane and launch the agent it
    /// recorded, continuing its last conversation in that folder where the
    /// CLI supports it. The coordinator is then asked to re-check its
    /// delegations.
    pub(crate) fn restart_task_agent(
        &mut self,
        task_id: String,
        member_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = restart_pane_key(&task_id, member_id.as_deref());
        // Never a second live agent next to a running one.
        if !self.missing_agents.contains(&key) {
            return;
        }
        let Some(task) = self.task_store.get(&task_id) else { return };
        let kind = self
            .panes_state
            .worktrees
            .get(&key)
            .and_then(|w| w.agent.clone())
            .unwrap_or_else(|| task.agent_kind.clone());
        let fresh = self.restart_tried.contains(&key);

        self.select(Selection::Task { task_id: task_id.clone(), member_id: member_id.clone() }, window, cx);
        self.continue_next_agent = !fresh;
        self.open_tab(TabKind::Agent(kind.clone()), window, cx);
        self.continue_next_agent = false;

        self.restart_tried.insert(key.clone());
        self.missing_agents.remove(&key);
        log::info!("restarted {kind} agent for {key} ({})", if fresh { "fresh" } else { "continue" });

        if member_id.is_none() && self.has_tmux {
            let Some(tab) = self.panes.get(&key).map(|p| p.active) else { return };
            let session = crate::tmux::session_name(&key, tab);
            cx.background_spawn(async move { nudge_when_running(&session) }).detach();
        }
        cx.notify();
    }
}

fn restart_pane_key(task_id: &str, member_id: Option<&str>) -> String {
    match member_id {
        Some(m) => a2a_broker::task_member_pane_key(task_id, m),
        None => a2a_broker::coordinator_pane_key(task_id),
    }
}

/// Wait (off the UI thread) for the restarted agent to be in the foreground,
/// give its UI a moment to take input, then send the re-check line. If the
/// agent never comes up (it exited back to the shell), send nothing.
fn nudge_when_running(session: &str) {
    use std::time::Duration;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(500));
        let running = crate::tmux::run_bounded(
            &["display-message", "-p", "-t", session, "#{pane_current_command}"],
            Duration::from_secs(1),
        )
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .is_some_and(|cmd| !cmd.is_empty() && !crate::tmux::is_shell(&cmd));
        if running {
            std::thread::sleep(Duration::from_secs(4));
            crate::tmux::send_keys_async(session, COORDINATOR_RESTART_NUDGE);
            return;
        }
    }
    log::warn!("restarted coordinator in {session} never came up; not sending the re-check");
}
