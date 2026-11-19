//! Agent signals in, alerts out: forge and agent-status updates, the pill
//! liveness check, notifications, and jumping from an
//! alert to its agent.

use super::*;

impl HarnessWorkspace {
    pub(crate) fn apply_forge(&mut self, update: ForgeUpdate, cx: &mut Context<Self>) {
        match update.result {
            Ok(status) => {
                self.forge_status.entry(update.key.repo_dir).or_default().insert(update.key.branch, status);
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

    /// Drop any `Running`/`Waiting` pill whose agent process is gone.
    ///
    /// The pill is a mirror of `.harness/status.json`, and an agent that is
    /// killed mid-dialog (tmux `kill-session`, a crash, Ctrl-C) never writes the
    /// resolving `"idle"` — so the pill would stick forever. The status file
    /// carries the writer's `pid`; if that process no longer exists we know the
    /// agent is gone and drop the pill to `Unknown` (which renders as IDLE).
    ///
    /// Runs on a slow timer. Only worktrees currently showing a non-terminal
    /// pill are probed, and a missing `pid` (an older agent, or a writer that
    /// doesn't stamp one) is left untouched — we never guess a process dead.
    pub(crate) fn reconcile_agent_liveness(&mut self, cx: &mut Context<Self>) {
        if self.liveness_pending {
            return;
        }
        // Resolve each non-terminal pill's tmux prefix here (it needs `self`);
        // the `ps` and tmux shell-outs then run off the render thread.
        let candidates: Vec<(PathBuf, Option<String>)> = self
            .agent_status
            .iter()
            .filter(|(_, s)| matches!(s, AgentState::Running | AgentState::Waiting))
            .map(|(root, _)| (root.clone(), self.session_prefix_for_path(root)))
            .collect();
        if candidates.is_empty() {
            return;
        }
        self.liveness_pending = true;
        let has_tmux = self.has_tmux;

        cx.spawn(async move |view, cx| {
            let dead = cx
                .background_spawn(async move { dead_agents(candidates, has_tmux) })
                .await;
            let _ = view.update(cx, |ws, cx| {
                ws.liveness_pending = false;
                let mut changed = false;
                for root in dead {
                    // Re-check: a fresh signal may have landed while the probe
                    // ran, and that one is newer evidence than our snapshot.
                    if matches!(ws.agent_status.get(&root), Some(AgentState::Running | AgentState::Waiting)) {
                        ws.conductor.stopped(&root);
                        ws.agent_since.insert(root.clone(), (std::time::SystemTime::now(), None));
                        ws.agent_status.insert(root, AgentState::Unknown);
                        changed = true;
                    }
                }
                if changed {
                    ws.remote_refresh(cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// The tmux session prefix that backs the tab set for the worktree (or main
    /// checkout) at `path`, or `None` if the path is neither. Mirrors the key
    /// `resolve()` / `open_tab` use, so the prefix matches the live session name.
    pub(crate) fn session_prefix_for_path(&self, path: &Path) -> Option<String> {
        for repo in &self.data.repos {
            if repo.repo_root == path {
                return Some(crate::tmux::session_prefix(&format!("main:{}", repo.name)));
            }
            if let Some(w) = repo.worktrees.iter().find(|w| w.path == path) {
                if let Some(tid) = &w.task_id {
                    return Some(crate::tmux::session_prefix(
                        &a2a_broker::task_member_pane_key(tid, &w.id),
                    ));
                }
                return Some(crate::tmux::session_prefix(&w.id));
            }
        }
        if let Some(task) = self.coordinator_task_for_path(path) {
            return Some(crate::tmux::session_prefix(
                &a2a_broker::coordinator_pane_key(&task.id),
            ));
        }
        for task in self.task_store.active() {
            for m in &task.members {
                if m.path == path {
                    return Some(crate::tmux::session_prefix(
                        &a2a_broker::task_member_pane_key(&task.id, &m.worktree_id),
                    ));
                }
            }
        }
        None
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
                        crate::tmux::session_prefix(&a2a_broker::task_member_pane_key(
                            tid, &w.id,
                        ))
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

    pub(crate) fn apply_agent(&mut self, signal: AgentSignal, cx: &mut Context<Self>) {
        let previous = self.agent_status.insert(signal.worktree_root.clone(), signal.state);
        let tool = signal.tool.as_ref().map(|t| t.name.clone());
        self.remote.record_signal(&signal.worktree_root, signal.kind.as_str(), signal.state, tool.clone());
        match self.agent_since.get_mut(&signal.worktree_root) {
            // Same state: keep its start time, follow the tool.
            Some(since) if previous == Some(signal.state) => since.1 = tool,
            _ => {
                self.agent_since.insert(
                    signal.worktree_root.clone(),
                    (std::time::SystemTime::now(), tool),
                );
            }
        }

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

        // Notify on any *transition* into Waiting, and only when the adapter's
        // signal is exact (R11) — a best-effort turn-complete dressed up as
        // "input needed" is the one thing that would make this noise.
        // The cache holds the fidelity of whatever adapter this session
        // spawned; signals for worktrees whose tab was never opened (app
        // restart, hooks left by an earlier session) fall back to the KIND's
        // declared fidelity — defaulting to TerminalOnly here silently
        // swallowed every notification after a restart.
        let fidelity = self
            .agent_fidelity
            .get(&signal.worktree_root)
            .copied()
            .unwrap_or_else(|| self.fidelity_for_kind(&signal.kind));
        // The conductor follows every agent, including the one on screen:
        // unlike a native notification, a bow you are watching isn't noise.
        let (label, repo, id) = self.alert_target(&signal.worktree_root);
        let label = super::ui::conductor::conductor_label(&label, &repo);
        let now = std::time::Instant::now();
        self.conductor.observe(
            &signal.worktree_root,
            signal.kind.clone(),
            signal.state,
            signal.reason,
            signal.session_file.clone(),
            signal.tool.clone(),
            label.clone(),
            now,
        );
        let nav = alert_nav_id(&id, &signal.kind);
        if previous != Some(signal.state) && signal.should_notify(fidelity) {
            let text = format!("{} needs your attention!", agent_display(&signal.kind));
            self.conductor.announce(&signal.worktree_root, label.clone(), text, nav.clone(), false, now);
        }
        if signal.should_notify_finished(previous, fidelity) {
            let text = format!("{} finished — your turn", agent_display(&signal.kind));
            self.conductor.announce(&signal.worktree_root, label, text, nav, true, now);
        }

        if previous != Some(signal.state) && signal.should_notify(fidelity) {
            self.notify_needs_input(&signal);
        }
        // "Finished — your turn" is a different claim with a different bar:
        // best-effort adapters may make it (it is exactly what turn-complete
        // means), and it only fires on the Running → Idle edge. This is the
        // notification the user reaches for when they leave a worktree tab
        // and ask an agent to work: silence here made the app feel deaf.
        if signal.should_notify_finished(previous, fidelity) {
            self.notify_finished(&signal);
        }
        self.remote_refresh(cx);
        cx.notify();
    }

    /// Post a native "needs input" notification, unless the user is already
    /// looking at that worktree — telling someone about the prompt they are
    /// staring at is pure noise (§4.6).
    pub(crate) fn notify_needs_input(&mut self, signal: &AgentSignal) {
        if self
            .resolve()
            .is_some_and(|active| active.path == signal.worktree_root)
        {
            return;
        }
        let (label, repo, id) = self.alert_target(&signal.worktree_root);
        crate::notifier::notify_agent(
            &agent_display(&signal.kind),
            &label,
            &repo,
            &alert_nav_id(&id, &signal.kind),
            crate::notifier::AgentAlert::NeedsAttention,
            &self.prefs.notify_sound,
        );
    }

    /// Post a "finished" notification, with the same view-suppression rule
    /// as needs-input: never narrate the screen the user is looking at.
    pub(crate) fn notify_finished(&mut self, signal: &AgentSignal) {
        if self
            .resolve()
            .is_some_and(|active| active.path == signal.worktree_root)
        {
            return;
        }
        let (label, repo, id) = self.alert_target(&signal.worktree_root);
        crate::notifier::notify_agent(
            &agent_display(&signal.kind),
            &label,
            &repo,
            &alert_nav_id(&id, &signal.kind),
            crate::notifier::AgentAlert::Finished,
            &self.prefs.notify_sound,
        );
    }

    /// Act on a notification click: select the worktree the alert was about
    /// and bring its agent tab to the front.
    ///
    /// A task's worktree is selected as that task with the member focused —
    /// the same thing clicking it in the sidebar does. The legacy
    /// `Selection::Worktree` the handler used before resolved to the worktree
    /// but not to its task, and the pane kept whatever tab was last active,
    /// so "View" often landed on a shell instead of the agent asking for you.
    pub(crate) fn navigate_to_alert(&mut self, nav: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (target, agent) = match nav.split_once(ALERT_AGENT_SEP) {
            Some((t, a)) => (t, Some(a)),
            None => (nav, None),
        };
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
                Some(task_id) => Selection::Task { task_id, member_id: Some(target.to_string()) },
                // A coordinator alert carries its task id: land on the task
                // itself, which is the coordinator.
                None if self.task_store.get(target).is_some() => {
                    Selection::Task { task_id: target.to_string(), member_id: None }
                }
                None => Selection::Worktree(target.to_string()),
            }
        };
        self.select(sel, window, cx);
        self.focus_agent_tab(agent, window, cx);
    }

    /// Make an agent tab active in the selected worktree's pane: the one
    /// running `kind` when there is one, else the first agent tab. A worktree
    /// with no agent tab is left on whatever it shows.
    pub(crate) fn focus_agent_tab(&mut self, kind: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(key) = self.resolve().map(|r| r.key) else { return };
        let Some(pane) = self.panes.get_mut(&key) else { return };
        let pick = kind
            .and_then(|k| pane.tabs.iter().find(|t| t.agent.as_ref().is_some_and(|a| a.as_str() == k)))
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
        if let Some(e) = self.manager.registry.entries().iter().find(|e| e.path == path) {
            return (e.name.clone(), e.repo.clone(), e.id.clone());
        }
        if let Some(r) = self.data.repos.iter().find(|r| r.repo_root == path) {
            return ("main".to_string(), r.name.clone(), format!("{MAIN_NAV_PREFIX}{}", r.name));
        }
        if let Some(task) = self.coordinator_task_for_path(path) {
            return (format!("{} · coordinator", task.name), String::new(), task.id.clone());
        }
        (crate::notifier::label_for("", path), String::new(), String::new())
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

/// The `pid` an agent stamped into `<root>/.harness/status.json`, if present.
/// `None` when the file is missing/malformed or the writer didn't record one —
/// callers treat that as "can't verify" and leave the pill alone.
pub(crate) fn status_pid(root: &Path) -> Option<u32> {
    #[derive(serde::Deserialize)]
    struct Probe {
        pid: Option<u32>,
    }
    let bytes = std::fs::read(root.join(".harness").join("status.json")).ok()?;
    serde_json::from_slice::<Probe>(&bytes).ok()?.pid
}

/// Whether a process with this pid currently exists. `kill(pid, 0)` sends no
/// signal; it just probes existence — 0 means alive, `ESRCH` means gone. Agents
/// run as us, so `EPERM` (exists, other user) can't arise here.
pub(crate) fn pid_alive(pid: u32) -> bool {
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// Which of `candidates` (root, tmux session prefix) no longer have a live
/// agent behind their running/waiting pill. Blocking: run off-thread.
pub(crate) fn dead_agents(candidates: Vec<(PathBuf, Option<String>)>, has_tmux: bool) -> Vec<PathBuf> {
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
    let panes = if has_tmux && !procs.is_empty() {
        crate::tmux::list_session_panes()
    } else {
        Vec::new()
    };
    let mut dead = Vec::new();
    for (root, prefix) in candidates {
        // Strongest signal: a stamped pid. Live is authoritative; gone is dead.
        if let Some(pid) = status_pid(&root) {
            if !pid_alive(pid) {
                dead.push(root);
            }
            continue;
        }
        if procs.is_empty() {
            continue;
        }
        let Some(prefix) = prefix else { continue };
        let pane_pids: Vec<u32> = panes
            .iter()
            .filter(|(session, _)| session.starts_with(&prefix))
            .map(|(_, pid)| *pid)
            .collect();
        // Either no live session, or the session is now just a shell.
        if !crate::resource_monitor::agent_running_in(&procs, &pane_pids) {
            dead.push(root);
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

/// The navigation id a notification carries: where to go, and which agent's
/// tab to front once there. An empty target (nothing resolvable) stays empty
/// so the click is a no-op rather than a jump to the wrong place.
pub(crate) fn alert_nav_id(target: &str, kind: &AgentKind) -> String {
    if target.is_empty() {
        return String::new();
    }
    format!("{target}{ALERT_AGENT_SEP}{}", kind.as_str())
}

#[cfg(test)]
mod alert_nav_tests {
    use super::{alert_nav_id, ALERT_AGENT_SEP};
    use agent_runtime::AgentKind;

    #[test]
    fn a_nav_id_carries_the_target_and_the_agent_and_splits_back() {
        let id = alert_nav_id("wt-42", &AgentKind::ClaudeCode);
        let (target, agent) = id.split_once(ALERT_AGENT_SEP).unwrap();
        assert_eq!(target, "wt-42");
        assert_eq!(agent, AgentKind::ClaudeCode.as_str());
        // The notifier makes request ids unique with `#<nanos>` and strips
        // from the first `#`; the separator must survive that.
        assert!(!id.contains('#'));
    }

    #[test]
    fn nothing_to_navigate_to_stays_empty() {
        assert_eq!(alert_nav_id("", &AgentKind::Pi), "");
    }
}
