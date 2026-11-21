//! Agent signals in, alerts out: forge and agent-status updates, the pill
//! liveness check, notifications, and jumping from an
//! alert to its agent.

use super::*;
use crate::conductor::AgentTarget;

impl HarnessWorkspace {
    pub(crate) fn apply_forge(&mut self, update: ForgeUpdate, cx: &mut Context<Self>) {
        match update.result {
            Ok(status) => {
                self.forge_status
                    .entry(update.key.repo_dir)
                    .or_default()
                    .insert(update.key.branch, status);
            }
            Err(_) => {
                if let Some(branches) = self.forge_status.get_mut(&update.key.repo_dir) {
                    branches.remove(&update.key.branch);
                }
            }
        }
        cx.notify();
    }

    pub(crate) fn branch_status(&self, repo: &Path, branch: &str) -> Option<&BranchStatus> {
        self.forge_status.get(repo)?.get(branch)
    }

    pub(crate) fn sync_forge_branches(&self) {
        if let Some(poller) = &self._poller {
            poller.set_branches(self.data.forge_branches());
        }
    }

    pub(crate) fn refresh_forge_repo(&mut self, repo: &Path) {
        if let Some(poller) = &self._poller {
            if let Some(branches) = self.forge_status.get_mut(repo) {
                for status in branches.values_mut() {
                    status.checks.clear();
                }
            }
            poller.refresh_repo(repo);
        }
    }

    /// Drop active statuses left by a tab whose agent process exited.
    pub(crate) fn reconcile_agent_liveness(&mut self, cx: &mut Context<Self>) {
        if self.liveness_pending {
            return;
        }
        let mut dead = Vec::new();
        let mut candidates = Vec::new();
        for target in self.conductor.active_targets() {
            let Some(key) = self.pane_key_for_path(&target.root) else {
                dead.push(target.clone());
                continue;
            };
            if !self.has_tmux && let Some(tab_id) = target.tab_id
                && self.agent_tab(&target.root, tab_id).is_none_or(|tab| {
                    matches!(&tab.content, TabContent::Terminal(pane) if pane.read(cx).direct_agent_exited())
                })
            {
                dead.push(target.clone());
                continue;
            }
            candidates.push((target.clone(), key));
        }
        if dead.is_empty() && candidates.is_empty() {
            return;
        }
        self.liveness_pending = true;
        let has_tmux = self.has_tmux;
        cx.spawn(async move |view, cx| {
            let dead = cx
                .background_spawn(async move {
                    dead.extend(dead_agents(candidates, has_tmux));
                    dead
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                ws.liveness_pending = false;
                if dead.is_empty() {
                    return;
                }
                for target in dead {
                    if matches!(
                        ws.conductor.state(&target),
                        Some(AgentState::Running | AgentState::Waiting)
                    ) {
                        ws.stop_agent(&target);
                    }
                }
                ws.remote_refresh(cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn stop_agent(&mut self, target: &AgentTarget) {
        self.conductor.stopped(target);
        self.agent_status
            .insert(target.root.clone(), self.conductor.root_state(&target.root));
    }

    /// The pane key for a main checkout, worktree, or task coordinator.
    fn pane_key_for_path(&self, path: &Path) -> Option<String> {
        for repo in &self.data.repos {
            if repo.repo_root == path {
                return Some(format!("main:{}", repo.name));
            }
            if let Some(w) = repo.worktrees.iter().find(|w| w.path == path) {
                return Some(match &w.task_id {
                    Some(tid) => a2a_broker::task_member_pane_key(tid, &w.id),
                    None => w.id.clone(),
                });
            }
        }
        if let Some(task) = self.coordinator_task_for_path(path) {
            return Some(a2a_broker::coordinator_pane_key(&task.id));
        }
        for task in self.task_store.active() {
            for m in &task.members {
                if m.path == path {
                    return Some(a2a_broker::task_member_pane_key(&task.id, &m.worktree_id));
                }
            }
        }
        None
    }

    fn agent_tab(&self, root: &Path, tab_id: u64) -> Option<&Tab> {
        let key = self.pane_key_for_path(root)?;
        self.panes
            .get(&key)?
            .tabs
            .iter()
            .find(|tab| tab.id == tab_id && tab.agent.is_some())
    }

    /// An evicted pane may still have live tmux tabs in its persisted record.
    fn has_agent_tab(&self, root: &Path, tab_id: u64) -> bool {
        let Some(key) = self.pane_key_for_path(root) else {
            return false;
        };
        if let Some(pane) = self.panes.get(&key) {
            return pane
                .tabs
                .iter()
                .any(|tab| tab.id == tab_id && tab.agent.is_some());
        }
        self.has_tmux
            && self.panes_state.worktrees.get(&key).is_some_and(|pane| {
                pane.tabs.iter().any(|tab| {
                    tab.id == tab_id
                        && matches!(tab.kind, crate::panes_state::TabRecordKind::Agent(_))
                })
            })
    }

    pub(crate) fn selected_agent(&self) -> Option<(AgentTarget, &AgentKind)> {
        let active = self.resolve()?;
        let tab = self.panes.get(&active.key)?.active_tab()?;
        Some((
            AgentTarget::new(&active.path, Some(tab.id)),
            tab.agent.as_ref()?,
        ))
    }

    pub(crate) fn dismiss_selected_agent(&mut self) {
        if let Some((target, kind)) = self.selected_agent() {
            let target = self.agent_selection_target(target, kind);
            self.conductor.dismiss(&target);
        }
    }

    /// Agent tabs of `kind` currently open under `root`.
    fn agent_tabs_of_kind(&self, root: &Path, kind: &AgentKind) -> usize {
        let Some(key) = self.pane_key_for_path(root) else {
            return 0;
        };
        self.panes
            .get(&key)
            .map(|pane| {
                pane.tabs
                    .iter()
                    .filter(|tab| tab.agent.as_ref() == Some(kind))
                    .count()
            })
            .unwrap_or(0)
    }

    /// The state of the agent in tab `tab_id` under `root`, when it has
    /// reported one. Per tab, so two agents in the same task or project each
    /// show their own state. Resolved the same way as the selected tab's.
    pub(crate) fn agent_tab_state(
        &self,
        root: &Path,
        tab_id: u64,
        kind: &AgentKind,
    ) -> Option<AgentState> {
        let target = self.agent_selection_target(AgentTarget::new(root, Some(tab_id)), kind);
        self.conductor.state(&target)
    }

    /// The conductor record for the selected tab. A legacy untagged record is
    /// used only when this tab is the only one of its kind: otherwise a tab
    /// that has not reported yet would dismiss a sibling's wait.
    pub(crate) fn agent_selection_target(
        &self,
        target: AgentTarget,
        kind: &AgentKind,
    ) -> AgentTarget {
        let sole = self.agent_tabs_of_kind(&target.root, kind) <= 1;
        self.conductor.selection_target(target, kind, sole)
    }

    /// Fold one raw sample into the current worktree list, producing the
    /// snapshot the sidebar band draws. Attribution is by tmux session prefix;
    /// main checkouts are first-class attributables alongside worktrees, so
    /// their sessions land in a row too rather than vanishing from the total.
    pub(crate) fn apply_resource(
        &mut self,
        raw: crate::resource_monitor::RawSample,
        cx: &mut Context<Self>,
    ) {
        use crate::resource_monitor::Attributable;
        let mut attributables: Vec<Attributable> = Vec::new();
        for repo in &self.data.repos {
            let main_key = format!("main:{}", repo.name);
            attributables.push(Attributable {
                label: format!("{} · main", repo.name),
                session_prefix: crate::tmux::session_prefix(&main_key),
            });
            for w in &repo.worktrees {
                let prefix = w
                    .task_id
                    .as_ref()
                    .map(|tid| {
                        crate::tmux::session_prefix(&a2a_broker::task_member_pane_key(tid, &w.id))
                    })
                    .unwrap_or_else(|| crate::tmux::session_prefix(&w.id));
                attributables.push(Attributable {
                    label: w.name.clone(),
                    session_prefix: prefix,
                });
            }
        }
        // Coordinators run in the task folder, not a worktree, so no repo
        // above covers their sessions.
        for task in self
            .task_store
            .active()
            .filter(|t| super::super::coordinator::has_coordinator(t))
        {
            attributables.push(Attributable {
                label: format!("{} · coordinator", task.name),
                session_prefix: crate::tmux::session_prefix(&a2a_broker::coordinator_pane_key(
                    &task.id,
                )),
            });
        }
        let snapshot = crate::resource_monitor::aggregate(
            &raw.procs,
            raw.app_pid,
            &raw.session_panes,
            &attributables,
        );
        self.resource_history.push(snapshot.total);
        self.resource = Some(snapshot);
        cx.notify();
    }

    pub(crate) fn apply_agent(
        &mut self,
        signal: AgentSignal,
        window_active: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = signal.tab_id {
            if !self.has_agent_tab(&signal.worktree_root, id) {
                return;
            }
        } else if signal.state == AgentState::Unknown {
            let target = AgentTarget::new(&signal.worktree_root, None);
            self.stop_agent(&target);
            // The phone cache is one entry per root and kind. Clearing it is
            // right when this legacy signal is that kind's only tab. A sibling
            // tab of the same kind shares the entry, so its last state stays.
            if self.agent_tabs_of_kind(&signal.worktree_root, &signal.kind) <= 1 {
                self.remote.record_signal(
                    &signal.worktree_root,
                    signal.kind.as_str(),
                    AgentState::Unknown,
                    None,
                );
                self.remote_refresh(cx);
            }
            cx.notify();
            return;
        }
        let target = AgentTarget::new(&signal.worktree_root, signal.tab_id);
        let tool = signal.tool.as_ref().map(|t| t.name.clone());
        self.remote.record_signal(
            &signal.worktree_root,
            signal.kind.as_str(),
            signal.state,
            tool,
        );

        if let Some(session) = &signal.session_file {
            self.remote
                .record_session(&signal.worktree_root, signal.kind.as_str(), session);
        }

        // Record the session file the moment an adapter reports one: it is the
        // harvesting hook M7 reads, and pi only learns it after startup.
        if let (Some(session), Some(id)) = (
            &signal.session_file,
            self.worktree_id_for_path(&signal.worktree_root),
        ) {
            let changed = self
                .manager
                .registry
                .get(&id)
                .is_none_or(|e| e.session_file.as_deref() != Some(session.as_path()));
            if changed {
                let _ = self
                    .manager
                    .registry
                    .set_session_file(&id, Some(session.clone()));
                let _ = self.manager.registry.save();
            }
        }

        let fidelity = self.fidelity_for_kind(&signal.kind);
        let (worktree, repo, id) = self.alert_target(&signal.worktree_root);
        let label = super::ui::conductor::conductor_label(&worktree, &repo);
        let now = std::time::Instant::now();
        let previous = self.conductor.observe(
            &target,
            signal.kind.clone(),
            signal.state,
            signal.reason,
            signal.session_file.clone(),
            signal.tool.clone(),
            label.clone(),
            now,
        );
        self.agent_status
            .insert(target.root.clone(), self.conductor.root_state(&target.root));

        let alert = if previous != Some(signal.state) && signal.should_notify(fidelity) {
            Some(crate::notifier::AgentAlert::NeedsAttention)
        } else if signal.should_notify_finished(previous, fidelity) {
            Some(crate::notifier::AgentAlert::Finished)
        } else {
            None
        };
        if let Some(alert) = alert {
            use crate::notifier::AgentAlert;
            let agent = agent_display(&signal.kind);
            let nav = alert_nav_id(&id, &signal.kind, target.tab_id);
            let (text, bow) = match alert {
                AgentAlert::NeedsAttention => (format!("{agent} needs your attention!"), false),
                AgentAlert::Finished => (format!("{agent} finished — your turn"), true),
            };
            self.conductor
                .announce(&target, label, text, nav.clone(), bow, now);
            // A different tab in the same worktree or an inactive app window
            // can still need a native notification.
            // A maximized tools panel omits the center pane, so the selected
            // agent tab is not on screen even though it is the active tab.
            let center_visible = !(self.prefs.diff_open && self.tools_maximized);
            let visible = window_active
                && center_visible
                && self.selected_agent().is_some_and(|(selected, kind)| {
                    selected.root == target.root
                        && kind == &signal.kind
                        && target.tab_id.is_none_or(|id| selected.tab_id == Some(id))
                });
            if !visible {
                crate::notifier::notify_agent(
                    &agent,
                    &worktree,
                    &repo,
                    &nav,
                    alert,
                    &self.prefs.notify_sound,
                );
            }
        }
        self.remote_refresh(cx);
        cx.notify();
    }

    /// Act on a notification click: select the worktree the alert was about
    /// and bring its agent tab to the front.
    ///
    /// A task's worktree is selected as that task with the member focused —
    /// the same thing clicking it in the sidebar does. The legacy
    /// `Selection::Worktree` the handler used before resolved to the worktree
    /// but not to its task, and the pane kept whatever tab was last active,
    /// so "View" often landed on a shell instead of the agent asking for you.
    pub(crate) fn navigate_to_alert(
        &mut self,
        nav: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut parts = nav.split(ALERT_AGENT_SEP);
        let target = parts.next().unwrap_or_default();
        let agent = parts.next();
        let tab_id = parts.next().and_then(|id| id.parse::<u64>().ok());
        let sel = if let Some(repo) = target.strip_prefix(MAIN_NAV_PREFIX) {
            Selection::Main(repo.to_string())
        } else {
            let task_id = self
                .data
                .repos
                .iter()
                .flat_map(|r| r.worktrees.iter())
                .find(|w| w.id == target)
                .and_then(|w| w.task_id.clone());
            match task_id {
                Some(task_id) => Selection::Task {
                    task_id,
                    member_id: Some(target.to_string()),
                },
                // A coordinator alert carries its task id: land on the task
                // itself, which is the coordinator.
                None if self.task_store.get(target).is_some() => Selection::Task {
                    task_id: target.to_string(),
                    member_id: None,
                },
                None => Selection::Worktree(target.to_string()),
            }
        };
        self.select(sel, window, cx);
        self.focus_agent_tab(agent, tab_id, window, cx);
    }

    /// Make an agent tab active in the selected worktree's pane: the one
    /// running `kind` when there is one, else the first agent tab. A worktree
    /// with no agent tab is left on whatever it shows.
    fn focus_agent_tab(
        &mut self,
        kind: Option<&str>,
        tab_id: Option<u64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.resolve().map(|r| r.key) else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&key) else {
            return;
        };
        let pick = tab_id
            .and_then(|id| pane.tabs.iter().find(|t| t.id == id && t.agent.is_some()))
            .or_else(|| {
                kind.and_then(|k| {
                    pane.tabs
                        .iter()
                        .find(|t| t.agent.as_ref().is_some_and(|a| a.as_str() == k))
                })
            })
            .or_else(|| pane.tabs.iter().find(|t| t.agent.is_some()))
            .map(|t| t.id);
        if let Some(id) = pick {
            pane.active = id;
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    /// Who an alert is about: `(label, repo, navigation id)`.
    ///
    /// The registry only holds worktrees, so an agent running in a project's
    /// *main checkout* used to resolve to nothing: the banner read
    /// "sample-repo () is waiting on you" (directory name, empty repo) and
    /// its View button carried an empty id that navigated nowhere. A main
    /// checkout is now named "main" in its project and navigates via a
    /// `main:<repo>` id, which the View handler maps to `Selection::Main`.
    pub(crate) fn alert_target(&self, path: &Path) -> (String, String, String) {
        if let Some(e) = self
            .manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == path)
        {
            return (e.name.clone(), e.repo.clone(), e.id.clone());
        }
        if let Some(r) = self.data.repos.iter().find(|r| r.repo_root == path) {
            return (
                "main".to_string(),
                r.name.clone(),
                format!("{MAIN_NAV_PREFIX}{}", r.name),
            );
        }
        if let Some(task) = self.coordinator_task_for_path(path) {
            return (
                format!("{} · coordinator", task.name),
                String::new(),
                task.id.clone(),
            );
        }
        (
            crate::notifier::label_for("", path),
            String::new(),
            String::new(),
        )
    }

    /// The active task whose coordinator runs in `path`, if any.
    pub(crate) fn coordinator_task_for_path(&self, path: &Path) -> Option<&worktrees::Task> {
        let tasks_root = self.data_dir.join("tasks");
        self.task_store.active().find(|t| {
            super::super::coordinator::has_coordinator(t)
                && worktrees::task_coordinator_dir(&tasks_root, &t.slug) == path
        })
    }

    /// Resolve a worktree path back to its registry id.
    pub(crate) fn worktree_id_for_path(&self, path: &Path) -> Option<String> {
        self.manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == path)
            .map(|e| e.id.clone())
    }

    // ---- selection resolution (None when no project is open) -------------
}

/// The `pid` an agent stamped into its status file, if present.
/// `None` when the file is missing/malformed or the writer didn't record one —
/// callers treat that as "can't verify" and leave the pill alone.
fn status_pid(target: &AgentTarget) -> Option<u32> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Probe {
        pid: Option<u32>,
        tab_id: Option<u64>,
    }
    let bytes = std::fs::read(agent_runtime::status_path(&target.root, target.tab_id)).ok()?;
    let probe = serde_json::from_slice::<Probe>(&bytes).ok()?;
    probe.pid.filter(|_| probe.tab_id == target.tab_id)
}

/// Whether a process with this pid currently exists. `kill(pid, 0)` sends no
/// signal; it just probes existence — 0 means alive, `ESRCH` means gone. Agents
/// run as us, so `EPERM` (exists, other user) can't arise here.
pub(crate) fn pid_alive(pid: u32) -> bool {
    // os_compat: kill(pid, 0) on Unix, OpenProcess + exit code on Windows.
    os_compat::process::is_alive(pid)
}

/// Which of `candidates` (root, tmux session prefix) no longer have a live
/// agent behind their running/waiting pill. Blocking: run off-thread.
fn dead_agents(candidates: Vec<(AgentTarget, String)>, has_tmux: bool) -> Vec<AgentTarget> {
    // Second liveness signal, for the common case where the status writer
    // stamps no pid (Claude Code and friends): is an actual agent process
    // still alive in the worktree's tmux session? A session that has fallen
    // back to a bare shell (`exec $SHELL`) leaves the last "waiting"/"running"
    // in status.json forever — this is what clears those stale pills. Empty
    // `procs` means `ps` failed; skip the tmux path rather than clear on no
    // evidence.
    let procs = if has_tmux {
        crate::resource_monitor::parse_ps(&ps_snapshot())
    } else {
        Vec::new()
    };
    // `None` is a failed listing, not "no panes". A candidate with no status
    // pid is left alone in that case, the same as when `ps` itself failed.
    let panes = if has_tmux && !procs.is_empty() {
        crate::tmux::list_session_panes().ok()
    } else {
        Some(Vec::new())
    };
    let mut dead = Vec::new();
    for (target, key) in candidates {
        // Strongest signal: a stamped pid. Live is authoritative; gone is dead.
        if let Some(pid) = status_pid(&target) {
            if !pid_alive(pid) {
                dead.push(target);
            }
            continue;
        }
        if procs.is_empty() || panes.is_none() {
            continue;
        }
        let panes = panes.as_deref().unwrap_or(&[]);
        let prefix = crate::tmux::session_prefix(&key);
        let session_name = target.tab_id.map(|id| crate::tmux::session_name(&key, id));
        let pane_pids: Vec<u32> = panes
            .iter()
            .filter(|(session, _)| match &session_name {
                Some(name) => session == name,
                None => session
                    .strip_prefix(&prefix)
                    .is_some_and(|suffix| suffix.parse::<u64>().is_ok()),
            })
            .map(|(_, pid)| *pid)
            .collect();
        // Either no live session, or the session is now just a shell.
        if !crate::resource_monitor::agent_running_in(&procs, &pane_pids) {
            dead.push(target);
        }
    }
    dead
}

/// A `pid ppid comm` snapshot of every process, for agent-liveness checks.
/// Empty on any error — callers treat that as "no evidence" and don't act on it.
pub(crate) fn ps_snapshot() -> String {
    std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Separates a notification's navigation target from the agent that raised
/// it. Not `#`: the notifier already uses that to make request ids unique.
pub(crate) const ALERT_AGENT_SEP: char = '|';

/// The navigation id holds the worktree, agent kind and optional exact tab.
pub(crate) fn alert_nav_id(target: &str, kind: &AgentKind, tab_id: Option<u64>) -> String {
    if target.is_empty() {
        return String::new();
    }
    match tab_id {
        Some(id) => format!(
            "{target}{ALERT_AGENT_SEP}{}{ALERT_AGENT_SEP}{id}",
            kind.as_str()
        ),
        None => format!("{target}{ALERT_AGENT_SEP}{}", kind.as_str()),
    }
}

#[cfg(test)]
mod alert_nav_tests {
    use super::{ALERT_AGENT_SEP, alert_nav_id};
    use agent_runtime::AgentKind;

    #[test]
    fn a_nav_id_carries_the_target_and_the_agent_and_splits_back() {
        let id = alert_nav_id("wt-42", &AgentKind::ClaudeCode, None);
        let (target, agent) = id.split_once(ALERT_AGENT_SEP).unwrap();
        assert_eq!(target, "wt-42");
        assert_eq!(agent, AgentKind::ClaudeCode.as_str());
        // The notifier makes request ids unique with `#<nanos>` and strips
        // from the first `#`; the separator must survive that.
        assert!(!id.contains('#'));
    }

    #[test]
    fn nothing_to_navigate_to_stays_empty() {
        assert_eq!(alert_nav_id("", &AgentKind::Pi, Some(7)), "");
    }

    #[test]
    fn sibling_codex_tabs_have_distinct_navigation_ids() {
        assert_eq!(
            alert_nav_id("main:app", &AgentKind::Codex, Some(7)),
            "main:app|codex|7"
        );
        assert_eq!(
            alert_nav_id("main:app", &AgentKind::Codex, Some(8)),
            "main:app|codex|8"
        );
    }
}
