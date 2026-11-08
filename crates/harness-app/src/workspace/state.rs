//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

/// The ref the diff panel measures from, for a given base and repo.
///
/// `Uncommitted` maps to `HEAD`, which collapses `GitWorktrees::diff`'s
/// `merge-base HEAD HEAD` to `HEAD` and leaves `git diff HEAD` — the working
/// tree. That is not a new code path: it is the one the main entry has always
/// taken, which is why main showed uncommitted work while tasks showed the
/// whole branch. The toggle makes the choice deliberate instead of a side
/// effect of which row you clicked.
pub(crate) fn diff_base_ref(base: DiffBase, compare_ref: &str) -> &str {
    match base {
        DiffBase::Uncommitted => "HEAD",
        DiffBase::DefaultBranch => compare_ref,
    }
}

impl HarnessWorkspace {

    pub(crate) fn apply_forge(&mut self, update: ForgeUpdate, cx: &mut Context<Self>) {
        match update.result {
            Ok(status) => {
                self.forge_status.insert(update.key.branch, status);
            }
            Err(_) => {
                self.forge_status.remove(&update.key.branch);
            }
        }
        cx.notify();
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
        let candidates: Vec<PathBuf> = self
            .agent_status
            .iter()
            .filter(|(_, s)| matches!(s, AgentState::Running | AgentState::Waiting))
            .map(|(root, _)| root.clone())
            .collect();
        if candidates.is_empty() {
            return;
        }

        // Second liveness signal, for the common case where the status writer
        // stamps no pid (Claude Code and friends): is an actual agent process
        // still alive in the worktree's tmux session? A session that has fallen
        // back to a bare shell (`exec $SHELL`) leaves the last "waiting"/"running"
        // in status.json forever — this is what clears those stale pills. Both
        // shell-outs run once per reconcile, only while a non-terminal pill
        // exists, so an idle app still does no work. Empty `procs` means `ps`
        // failed; skip the tmux path entirely rather than clear on no evidence.
        let procs = if self.has_tmux {
            crate::resource_monitor::parse_ps(&ps_snapshot())
        } else {
            Vec::new()
        };
        let panes = if self.has_tmux && !procs.is_empty() {
            crate::tmux::list_session_panes()
        } else {
            Vec::new()
        };

        let mut changed = false;
        for root in candidates {
            // Strongest signal: a stamped pid that no longer exists.
            if let Some(pid) = status_pid(&root) {
                if !pid_alive(pid) {
                    self.conductor.stopped(&root);
                    self.agent_status.insert(root, AgentState::Unknown);
                    changed = true;
                }
                // A stamped, live pid is authoritative — trust it, skip the rest.
                continue;
            }
            // No pid: fall back to tmux session liveness (skip if `ps` failed).
            if procs.is_empty() {
                continue;
            }
            let Some(prefix) = self.session_prefix_for_path(&root) else {
                continue;
            };
            let pane_pids: Vec<u32> = panes
                .iter()
                .filter(|(session, _)| session.starts_with(&prefix))
                .map(|(_, pid)| *pid)
                .collect();
            // Either the worktree has no live session, or the session is now
            // just a shell — the agent that wrote this pill is gone.
            if !crate::resource_monitor::agent_running_in(&procs, &pane_pids) {
                self.conductor.stopped(&root);
                self.agent_status.insert(root, AgentState::Unknown);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The tmux session prefix that backs the tab set for the worktree (or main
    /// checkout) at `path`, or `None` if the path is neither. Mirrors the key
    /// `resolve()` / `open_tab` use, so the prefix matches the live session name.
    fn session_prefix_for_path(&self, path: &Path) -> Option<String> {
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
            self.notify_needs_input(&signal, cx);
        }
        // "Finished — your turn" is a different claim with a different bar:
        // best-effort adapters may make it (it is exactly what turn-complete
        // means), and it only fires on the Running → Idle edge. This is the
        // notification the user reaches for when they leave a worktree tab
        // and ask an agent to work: silence here made the app feel deaf.
        if signal.should_notify_finished(previous, fidelity) {
            self.notify_finished(&signal, cx);
        }
        cx.notify();
    }

    /// Wake the conductor: show the pet card with the message, and keep its
    /// arms moving until it expires or is closed. A fresh notification while
    /// visible just replaces the message and extends the performance.
    pub(crate) fn show_pet(&mut self, message: String, cx: &mut Context<Self>) {
        // With the always-on conductor up, he announces it himself; a
        // second, smaller maestro popping up beside him would be silly.
        if !self.prefs.pet_enabled || self.prefs.conductor_always_on {
            return;
        }
        let until = std::time::Instant::now() + Duration::from_secs(8);
        let animating = self.pet.is_some();
        self.pet = Some((message, until));
        cx.notify();
        if animating {
            return; // the running loop picks up the new expiry
        }
        cx.spawn(async move |view, cx| loop {
            cx.background_executor().timer(Duration::from_millis(350)).await;
            let done = view.update(cx, |ws, cx| {
                match &ws.pet {
                    Some((_, until)) if *until > std::time::Instant::now() => {
                        ws.pet_pose = !ws.pet_pose;
                        cx.notify();
                        false
                    }
                    _ => {
                        ws.pet = None;
                        cx.notify();
                        true
                    }
                }
            });
            if done.unwrap_or(true) {
                break;
            }
        })
        .detach();
    }

    /// Post a native "needs input" notification, unless the user is already
    /// looking at that worktree — telling someone about the prompt they are
    /// staring at is pure noise (§4.6).
    pub(crate) fn notify_needs_input(&mut self, signal: &AgentSignal, cx: &mut Context<Self>) {
        if self
            .resolve()
            .is_some_and(|active| active.path == signal.worktree_root)
        {
            return;
        }
        let (label, repo, id) = self.alert_target(&signal.worktree_root);
        self.show_pet(
            format!("{} needs your attention!", agent_display(&signal.kind)),
            cx,
        );
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
    pub(crate) fn notify_finished(&mut self, signal: &AgentSignal, cx: &mut Context<Self>) {
        if self
            .resolve()
            .is_some_and(|active| active.path == signal.worktree_root)
        {
            return;
        }
        let (label, repo, id) = self.alert_target(&signal.worktree_root);
        self.show_pet(
            format!("{} finished — your turn", agent_display(&signal.kind)),
            cx,
        );
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
                None => Selection::Worktree(target.to_string()),
            }
        };
        self.select(sel, window, cx);
        self.focus_agent_tab(agent, window, cx);
    }

    /// Make an agent tab active in the selected worktree's pane: the one
    /// running `kind` when there is one, else the first agent tab. A worktree
    /// with no agent tab is left on whatever it shows.
    fn focus_agent_tab(&mut self, kind: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
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
    /// "msrosa-tutoring () is waiting on you" (directory name, empty repo) and
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
        (crate::notifier::label_for("", path), String::new(), String::new())
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

    pub(crate) fn resolve(&self) -> Option<Resolved> {
        let primary = self.data.repos.first()?;
        let main_of = |r: &crate::data::RepoView| Resolved {
            key: format!("main:{}", r.name),
            path: r.repo_root.clone(),
            editor_path: r.repo_root.clone(),
            agent_kind: None,
            compare_ref: "HEAD".to_string(),
            branch: r.head_branch.clone(),
            task_id: None,
        };
        match &self.selection {
            Selection::Main(repo) => {
                let r = self.data.repos.iter().find(|x| &x.name == repo).unwrap_or(primary);
                Some(main_of(r))
            }
            Selection::Worktree(id) => {
                // Prefer task selection when the worktree belongs to a task.
                if let Some(entry) = self.manager.registry.get(id)
                    && let Some(tid) = &entry.task_id
                {
                    return self.resolve_task(tid, Some(id));
                }
                for r in &self.data.repos {
                    if let Some(w) = r.worktrees.iter().find(|w| &w.id == id) {
                        return Some(Resolved {
                            key: id.clone(),
                            path: w.path.clone(),
                            editor_path: w.path.clone(),
                            agent_kind: Some(
                                self.manager
                                    .registry
                                    .get(id)
                                    .map(|e| e.agent_kind.clone())
                                    .unwrap_or_default(),
                            ),
                            compare_ref: r.compare_ref.clone(),
                            branch: w.branch.clone(),
                            task_id: None,
                        });
                    }
                }
                Some(main_of(primary))
            }
            Selection::Task {
                task_id,
                member_id,
            } => self.resolve_task(task_id, member_id.as_deref()),
        }
    }

    /// Switch what the diff panel measures against, and recompute.
    ///
    /// App-wide and persisted: one setting to reason about, and it survives a
    /// restart. `recompute_diff` bumps `diff_gen`, so a computation still in
    /// flight for the base just left is discarded rather than landing late.
    pub(crate) fn set_diff_base(&mut self, base: DiffBase, cx: &mut Context<Self>) {
        self.diff_base_menu = None;
        if self.prefs.diff_base == base {
            cx.notify();
            return;
        }
        self.prefs.diff_base = base;
        self.save_prefs(cx);
        self.recompute_diff(cx);
        cx.notify();
    }

    fn resolve_task(&self, task_id: &str, member_id: Option<&str>) -> Option<Resolved> {
        let task = self.task_store.get(task_id)?;
        let home = task.home_member()?;
        let focused = member_id
            .and_then(|id| task.members.iter().find(|m| m.worktree_id == id))
            .unwrap_or(home);
        let compare_ref = self
            .data
            .repos
            .iter()
            .find(|r| r.name == focused.repo)
            .map(|r| r.compare_ref.clone())
            .unwrap_or_else(|| "HEAD".into());
        // Per-member context: agent/shell cwd and pane key follow the focused
        // member (home when none selected). Unified DIFF stays task-scoped.
        Some(Resolved {
            key: a2a_broker::task_member_pane_key(task_id, &focused.worktree_id),
            path: focused.path.clone(),
            editor_path: focused.path.clone(),
            agent_kind: Some(task.agent_kind.clone()),
            compare_ref,
            branch: focused.branch.clone(),
            task_id: Some(task_id.to_string()),
        })
    }

    /// `HARNESS_*` for a resolved target, as `new-session -e` pairs.
    ///
    /// This is the same context hook scripts get, handed to the tmux session so
    /// the pane's shell inherits it instead of having `export` lines typed into
    /// it after setup runs. Empty for a target with no task: the vars describe a
    /// task's worktree layout and mean nothing on a bare `main` checkout.
    pub(crate) fn pane_env(&self, r: &Resolved) -> Vec<(String, String)> {
        let Some(task_id) = &r.task_id else {
            return Vec::new();
        };
        let Some(task) = self.task_store.get(task_id) else {
            return Vec::new();
        };
        let member = match task
            .members
            .iter()
            .find(|m| m.path == r.path)
            .or_else(|| task.home_member())
        {
            Some(m) => m,
            None => return Vec::new(),
        };
        let Some(entry) = self.manager.registry.get(&member.worktree_id) else {
            return Vec::new();
        };
        let env = worktrees::HookEnv {
            repo_root: entry.repo_root.clone(),
            worktree: member.path.clone(),
            branch: member.branch.clone(),
            task_slug: task.slug.clone(),
            task_id: Some(task_id.clone()),
            home_worktree: task.home_path().map(|p| p.to_path_buf()),
        };
        let mut pairs: Vec<(String, String)> = env.to_map().into_iter().collect();
        let tasks_root = self.data_dir.join("tasks");
        if let Ok(Some(path)) = worktrees::ensure_task_agent_context(task, &tasks_root) {
            pairs.push((
                agent_runtime::HARNESS_TASK_CONTEXT.to_string(),
                path.to_string_lossy().into_owned(),
            ));
        }
        // A2A broker endpoint for multi-member orchestration (when running).
        if let Some(info) = self.task_brokers.get(task_id) {
            pairs.push(("HARNESS_A2A_URL".into(), info.base_url.clone()));
            pairs.push(("HARNESS_A2A_KEY".into(), info.api_key.clone()));
            pairs.push((
                "HARNESS_A2A_TENANT".into(),
                member.worktree_id.clone(),
            ));
        }
        pairs
    }

    pub(crate) fn ensure_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        self.pane_clock = self.pane_clock.wrapping_add(1);
        if let Some(existing) = self.panes.get_mut(&r.key) {
            existing.last_used = self.pane_clock;
            return;
        }

        let restored = self.restore_tabs(&r.key, window, cx);
        if !restored {
            self.open_tab(TabKind::Shell, window, cx);
        }
        self.evict_stale_panes(&r.key);
    }

    /// Fidelity for notifications: prefs signal profile for custom agents,
    /// else the kind's built-in declaration. Used when the per-worktree cache
    /// is cold (app restart before any agent tab was opened).
    pub(crate) fn fidelity_for_kind(&self, kind: &AgentKind) -> WaitFidelity {
        if let Some(signal) = self
            .prefs
            .agents
            .get(kind.as_str())
            .and_then(|p| p.signal.as_ref())
        {
            return signal.fidelity;
        }
        agent_runtime::fidelity_for(kind)
    }

    /// Prepare the worktree's agent adapter and return the command to run.
    ///
    /// Separate from tab creation because it also wires up the status signal
    /// and the local git excludes, which are per-worktree, not per-tab.
    /// Prepare an adapter and command for a *specific* agent kind — the ＋
    /// menu offers all enabled agents, not only the worktree's default.
    ///
    /// A user command override replaces the launch command but NOT the
    /// adapter wiring: hooks, excludes, and the status watch still belong to
    /// the kind, or an overridden Claude Code would silently lose its status
    /// pill and notifications.
    pub(crate) fn agent_command_for(
        &mut self,
        kind: &AgentKind,
        r: &Resolved,
    ) -> Option<agent_runtime::AgentCommand> {
        let signal = self
            .prefs
            .agents
            .get(kind.as_str())
            .and_then(|p| p.signal.clone());
        let runtime =
            agent_runtime::runtime_for_with_signal(kind, &r.path, &self.state_dir, signal);
        if let Err(e) = runtime.on_spawn(&r.path) {
            log::warn!("{kind} setup failed for {}: {e}", r.path.display());
        }
        // The harness runs inside the user's own repository, so anything it
        // drops there would otherwise show up as uncommitted changes in their
        // project. Exclude locally rather than touching their .gitignore.
        if let Err(e) = self
            .manager
            .git()
            .exclude_locally(&r.path, runtime.ignored_paths())
        {
            log::warn!("could not add local git excludes for {}: {e}", r.path.display());
        }
        self.agent_fidelity.insert(r.path.clone(), runtime.fidelity());
        // Adapters that stream their own signals (OpenCode's SSE) publish
        // straight onto the shared bus; file-based ones are served by
        // StatusFileSource.
        if let Some(rx) = runtime.subscribe() {
            let bus = self.bus.clone();
            std::thread::Builder::new()
                .name("agent-signal-forward".into())
                .spawn(move || {
                    for signal in rx {
                        bus.publish(signal);
                    }
                })
                .ok();
        }
        // Resolve the adapter command first so a prefs override can keep the
        // managed env (HARNESS_MANAGED / HARNESS_AGENT) that hooks need.
        let runtime_cmd = runtime.command(&r.path);
        let runtime_env = runtime_cmd
            .as_ref()
            .map(|c| c.env.clone())
            .unwrap_or_default();

        let context_path = r.task_id.as_ref().and_then(|tid| {
            let task = self.task_store.get(tid)?;
            let tasks_root = self.data_dir.join("tasks");
            worktrees::ensure_task_agent_context(task, &tasks_root)
                .ok()
                .flatten()
        });

        if let Some(cmd) = self
            .prefs
            .agents
            .get(kind.as_str())
            .and_then(|p| p.command.clone())
        {
            // Login + interactive: an override is a line the user typed, so it
            // has to resolve the way it would in their terminal — including
            // aliases and shell functions, which only exist once `.zshrc` runs.
            let launch = match &context_path {
                Some(path) => agent_runtime::shell_command_with_task_context(&cmd, path),
                None => cmd,
            };
            let (shell, args) = worktrees::interactive_login_command(&launch);
            let mut env = runtime_env;
            if let Some(path) = &context_path {
                env.retain(|(k, _)| k != agent_runtime::HARNESS_TASK_CONTEXT);
                env.push((
                    agent_runtime::HARNESS_TASK_CONTEXT.to_string(),
                    path.to_string_lossy().into_owned(),
                ));
            }
            return Some(agent_runtime::AgentCommand {
                program: shell.into(),
                args,
                env,
                label: kind.as_str().to_string(),
            });
        }
        if runtime_cmd.is_none() {
            log::info!("{kind} not on PATH — falling back to a plain shell");
        }
        let mut runtime_cmd = runtime_cmd;
        if let (Some(cmd), Some(path)) = (runtime_cmd.as_mut(), context_path.as_ref()) {
            agent_runtime::apply_task_context(cmd, kind, path);
        }
        runtime_cmd
    }

    /// Open a new tab in the active worktree's set and focus it.
    pub(crate) fn open_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        // `main` is a read-only checkout with no agent (D8).
        // D8 revised (July 22, 2026): agents may run on main too — it is the
        // user's repo and their call. Worktrees remain the *recommended*
        // isolation, and the chat pane stays worktree-only (it is pi's RPC
        // surface, wired to a worktree session).
        if matches!(kind, TabKind::Chat) && r.agent_kind.is_none() {
            self.note = "agent chat needs a worktree".to_string();
            cx.notify();
            return;
        }

        let tab_id = self.next_tab_id();

        let (content, title) = match kind {
            TabKind::Shell => {
                let session = self.has_tmux.then(|| crate::terminal_pane::TerminalSession {
                    name: crate::tmux::session_name(&r.key, tab_id),
                    attach_only: false,
                    env: self.pane_env(&r),
                });
                (
                    TabContent::Terminal(
                        cx.new(|cx| TerminalPane::new(r.path.clone(), None, session, window, cx)),
                    ),
                    "❯_ shell".to_string(),
                )
            }
            TabKind::Agent(ref agent) => {
                let command = self.agent_command_for(agent, &r);
                let label = command
                    .as_ref()
                    .map(|c| c.label.clone())
                    .unwrap_or_else(|| "shell".to_string());
                let session = self.has_tmux.then(|| crate::terminal_pane::TerminalSession {
                    name: crate::tmux::session_name(&r.key, tab_id),
                    attach_only: false,
                    env: self.pane_env(&r),
                });
                (
                    TabContent::Terminal(cx.new(|cx| {
                        TerminalPane::new(r.path.clone(), command, session, window, cx)
                    })),
                    label,
                )
            }
            // Nothing to construct: the surface reads state the view already
            // holds, so the tab is a marker saying where to draw it.
            TabKind::Diff => (TabContent::Diff, "⑂ diff".to_string()),
            TabKind::Editor => {
                // One per worktree: a second ⌘E switches to the existing tab.
                if let Some(pane) = self.panes.get_mut(&r.key)
                    && let Some(existing) =
                        pane.tabs.iter().find(|t| matches!(t.content, TabContent::Editor(_)))
                {
                    pane.active = existing.id;
                    self.focus_active_pane(window, cx);
                    cx.notify();
                    return;
                }
                let Some(ws) = self.ensure_ide_workspace(window, cx) else {
                    self.note = "could not open the editor for this worktree".to_string();
                    cx.notify();
                    return;
                };
                (TabContent::Editor(ws), "{ } editor".to_string())
            }
            TabKind::Chat => {
                let path = r.path.clone();
                (
                    TabContent::Chat(cx.new(|cx| crate::chat_pane::ChatPane::new(path, cx))),
                    "◆ chat".to_string(),
                )
            }
        };

        let agent_of_kind = match &kind {
            TabKind::Agent(a) => Some(a.clone()),
            _ => None,
        };
        // Create the pane if it does not exist yet. This was `if let Some`,
        // and the else case silently dropped the tab — including a fully
        // constructed zed Workspace, whose own diagnostics all read healthy
        // because the entity *was* healthy right up until it was released.
        // Every state probe passed while the screen showed nothing; a tab
        // that cannot be attached must never be discarded without a trace.
        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self.panes.entry(r.key.clone()).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        pane.last_used = self.pane_clock;
        pane.tabs.push(Tab { id: tab_id, title, content, agent: agent_of_kind });
        pane.active = tab_id;
        let opened_agent_key = matches!(kind, TabKind::Agent(_)).then(|| r.key.clone());
        self.focus_active_pane(window, cx);
        self.save_panes();
        if let Some(key) = opened_agent_key {
            self.flush_a2a_inbox_for_pane(&key);
            // New tmux sessions are not always listed instantly; retry shortly.
            let retry_key = key.clone();
            cx.spawn(async move |view, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(750))
                    .await;
                let _ = view.update(cx, |ws, _cx| {
                    ws.flush_a2a_inbox_for_pane(&retry_key);
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// The user tried to edit a diff line. A diff is a picture of a change, not
    /// the file, so the only useful response is to take them somewhere they can
    /// actually type — open (or focus) that file as a harness tab.
    pub(crate) fn request_edit(
        &mut self,
        file: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A deleted file has nothing on disk to open. Say so rather than
        // raising a question whose every answer fails.
        if self
            .diff
            .files
            .iter()
            .any(|f| f.path == file && f.change == Change::Deleted)
        {
            self.note = format!("{file} is deleted in this branch");
            cx.notify();
            return;
        }
        self.open_in_editor(file, line, window, cx);
    }

    /// Open `file` at `line` via the service Workspace; the item-sync path
    /// promotes it to a harness file tab.
    pub(crate) fn open_in_editor(
        &mut self,
        file: String,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.exit_hooks_edit(window, cx);
        self.edit_prompt = None;
        let Some(r) = self.resolve() else { return };
        let Some(hosted) = self.ensure_service_workspace(window, cx) else {
            self.note = "could not open an editor".to_string();
            cx.notify();
            return;
        };
        crate::workspace_host::open_file_at(&hosted, &r.path, &file, line, false, window, cx);
        cx.notify();
    }

    /// Show or hide the right-hand tools panel (Files / Git / Review).
    ///
    /// Always available so Files/Git stay reachable even when Review lives in a
    /// center tab.
    pub(crate) fn toggle_diff(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.prefs.diff_open = !self.prefs.diff_open;
        self.save_prefs(cx);
        cx.notify();
    }

    /// Select a tools-panel tab, open the panel if needed, and focus the surface.
    pub(crate) fn set_tool_tab(
        &mut self,
        tab: ToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefs.tool_tab = tab;
        self.prefs.diff_open = true;
        match tab {
            ToolTab::Files => {
                let _ = self.ensure_service_workspace(window, cx);
                match self.files_sub_tab {
                    FilesSubTab::Explorer => {
                        self.activate_hosted_tool_panel(ToolTab::Files, window, cx);
                    }
                    FilesSubTab::Search => {
                        self.ensure_project_search_ui(window, cx);
                        if let Some(ui) = self.project_search.as_ref() {
                            window.focus(&ui.query.focus_handle(cx), cx);
                        }
                    }
                }
            }
            ToolTab::Git => {
                // Keep the current center tab (Terminal/Chat/…); tools host the
                // service Workspace without promoting an Editor chrome tab.
                let _ = self.ensure_service_workspace(window, cx);
                self.activate_hosted_tool_panel(tab, window, cx);
            }
            ToolTab::Review if self.prefs.diff_location == DiffLocation::Tab => {
                self.open_tab(TabKind::Diff, window, cx);
            }
            ToolTab::Review => {
                window.focus(&self.diff_surface_focus, cx);
            }
        }
        self.save_prefs(cx);
        cx.notify();
    }

    /// ⌘⇧E / ⌘E — open tools on Files → Explorer and focus the tree.
    pub(crate) fn focus_files_tool(
        &mut self,
        _: &crate::FocusFilesTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files_sub_tab = FilesSubTab::Explorer;
        self.set_tool_tab(ToolTab::Files, window, cx);
    }

    /// ⌘⇧F — open tools on Files → Search and focus the query field.
    pub(crate) fn focus_project_search(
        &mut self,
        _: &crate::FocusProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.resolve().is_none() {
            self.note = "select a project first".into();
            cx.notify();
            return;
        }
        self.files_sub_tab = FilesSubTab::Search;
        self.prefs.tool_tab = ToolTab::Files;
        self.prefs.diff_open = true;
        let _ = self.ensure_service_workspace(window, cx);
        self.ensure_project_search_ui(window, cx);
        if let Some(ui) = self.project_search.as_ref() {
            window.focus(&ui.query.focus_handle(cx), cx);
        }
        self.save_prefs(cx);
        cx.notify();
    }

    /// ⌘⇧G — open tools on Git and focus the panel.
    pub(crate) fn focus_git_tool(
        &mut self,
        _: &crate::FocusGitTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_tool_tab(ToolTab::Git, window, cx);
    }

    /// Open tools on Review (and focus the diff surface).
    pub(crate) fn focus_review_tool(
        &mut self,
        _: &crate::FocusReviewTool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_tool_tab(ToolTab::Review, window, cx);
    }

    /// Mark the hosted Files/Git panel active and focus it; keep docks closed.
    pub(crate) fn activate_hosted_tool_panel(
        &mut self,
        tab: ToolTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui::Focusable as _;
        use workspace::Panel as _;

        let Some(hosted) = self.editor_workspace() else {
            return;
        };
        hosted.update(cx, |ws, cx| {
            for dock in ws.all_docks() {
                if dock.read(cx).is_open() {
                    dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
                }
            }
        });
        match tab {
            ToolTab::Files => {
                if let Some(panel) = hosted.read(cx).panel::<project_panel::ProjectPanel>(cx) {
                    panel.update(cx, |p, cx| {
                        p.set_active(true, window, cx);
                        window.focus(&p.focus_handle(cx), cx);
                    });
                }
            }
            ToolTab::Git => {
                if let Some(panel) = hosted.read(cx).panel::<git_ui::git_panel::GitPanel>(cx) {
                    panel.update(cx, |p, cx| {
                        p.set_active(true, window, cx);
                        window.focus(&p.focus_handle(cx), cx);
                    });
                }
            }
            ToolTab::Review => {}
        }
    }

    /// Service Workspace for the active selection, if already created.
    /// The full-editor workspace for the selected worktree, created on first
    /// use over the *service* workspace's project (see
    /// `workspace_host::ide_workspace_for` for why they share it).
    pub(crate) fn ensure_ide_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        if let Some(ws) = self.ide_workspaces.get(&key) {
            return Some(ws.clone());
        }
        let service = self.ensure_service_workspace(window, cx)?;
        let project = service.read(cx).project().clone();
        match crate::workspace_host::ide_workspace_for(project, window, cx) {
            Ok(ws) => {
                self.ide_workspaces.insert(key, ws.clone());
                Some(ws)
            }
            Err(e) => {
                log::warn!("could not open the editor workspace: {e}");
                None
            }
        }
    }

    /// The selected worktree's active Editor tab workspace, if that tab is
    /// the one showing.
    pub(crate) fn active_ide_workspace(&self) -> Option<Entity<::workspace::Workspace>> {
        let r = self.resolve()?;
        match &self.panes.get(&r.key)?.active_tab()?.content {
            TabContent::Editor(ws) => Some(ws.clone()),
            _ => None,
        }
    }

    pub(crate) fn editor_workspace(&self) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        self.service_workspaces.get(&key).cloned()
    }

    /// Create or return the per-worktree service Workspace (Files/Git + file tabs).
    pub(crate) fn ensure_service_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let r = self.resolve()?;
        let key = r.key.clone();
        if let Some(ws) = self.service_workspaces.get(&key) {
            return Some(ws.clone());
        }
        match crate::workspace_host::workspace_for(&r.editor_path, window, cx) {
            Ok(hosted) => {
                crate::workspace_host::probe(&hosted, window, cx);
                // Hide Zed's nested tab bar — harness owns the strip.
                self.wire_service_workspace(&hosted, window, cx);
                self.service_workspaces.insert(key, hosted.clone());
                // Ensure a pane set exists so file tabs have somewhere to land.
                self.pane_clock = self.pane_clock.wrapping_add(1);
                self.panes.entry(r.key.clone()).or_insert_with(|| PaneSet {
                    tabs: Vec::new(),
                    active: 0,
                    last_used: 0,
                }).last_used = self.pane_clock;
                cx.notify();
                Some(hosted)
            }
            Err(e) => {
                log::warn!("could not open project for {}: {e}", r.editor_path.display());
                self.note = format!("could not open project: {e}");
                cx.notify();
                None
            }
        }
    }

    /// Alias used by the tools empty-state button.
    pub(crate) fn ensure_editor_workspace_quiet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        self.ensure_service_workspace(window, cx)
    }

    /// Subscribe to a hosted Zed Workspace so harness file tabs track preview
    /// ↔ permanent the same way Zed's own tab bar does (edit / double-click).
    pub(crate) fn wire_service_workspace(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        hosted.update(cx, |ws, cx| {
            for pane in ws.panes() {
                pane.update(cx, |pane, _| {
                    pane.set_should_display_tab_bar(|_, _| false);
                });
            }
        });
        let panes: Vec<_> = hosted.read(cx).panes().iter().cloned().collect();
        for pane in &panes {
            self.observe_service_pane_preview(pane, cx);
        }
        cx.observe(hosted, |this, _, cx| {
            this.sync_file_preview_flags(cx);
            cx.notify();
        })
        .detach();
        cx.subscribe_in(hosted, window, {
            move |this, hosted, event: &workspace::Event, window, cx| {
                match event {
                    workspace::Event::ItemAdded { item } => {
                        this.on_service_item_added(hosted, item.as_ref(), window, cx);
                    }
                    workspace::Event::ItemRemoved { item_id } => {
                        this.on_service_item_removed(*item_id, window, cx);
                    }
                    workspace::Event::ActiveItemChanged => {
                        this.on_service_active_item_changed(hosted, window, cx);
                    }
                    workspace::Event::PaneAdded(pane) => {
                        pane.update(cx, |pane, _| {
                            pane.set_should_display_tab_bar(|_, _| false);
                        });
                        this.observe_service_pane_preview(pane, cx);
                    }
                    _ => {}
                }
            }
        })
        .detach();
    }

    fn observe_service_pane_preview(
        &mut self,
        pane: &Entity<::workspace::Pane>,
        cx: &mut Context<Self>,
    ) {
        // Edits clear `preview_item_id` on the pane; observe so italics drop
        // without waiting for ActiveItemChanged.
        cx.observe(pane, |this, _, cx| {
            this.sync_file_preview_flags(cx);
            cx.notify();
        })
        .detach();
    }

    /// Mirror Zed's `preview_item_id` onto harness `TabContent::File.preview`.
    pub(crate) fn sync_file_preview_flags(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else {
            return;
        };
        let Some(hosted) = self.service_workspaces.get(&r.key).cloned() else {
            return;
        };
        let Some(pane_set) = self.panes.get_mut(&r.key) else {
            return;
        };
        let mut changed = false;
        for tab in &mut pane_set.tabs {
            let TabContent::File { item_id, preview } = &mut tab.content else {
                continue;
            };
            let is_preview = hosted.read(cx).panes().iter().any(|pane| {
                pane.read(cx).is_active_preview_item(*item_id)
            });
            if *preview != is_preview {
                *preview = is_preview;
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Double-click a harness file tab → pin it (Zed `unpreview_item_if_preview`).
    pub(crate) fn pin_file_tab(
        &mut self,
        tab_id: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else {
            return;
        };
        let item_id = {
            let Some(pane) = self.panes.get(&r.key) else {
                return;
            };
            let Some(tab) = pane.tabs.iter().find(|t| t.id == tab_id) else {
                return;
            };
            match &tab.content {
                TabContent::File {
                    item_id,
                    preview: true,
                } => *item_id,
                _ => return,
            }
        };
        if let Some(hosted) = self.service_workspaces.get(&r.key).cloned() {
            hosted.update(cx, |ws, cx| {
                for pane in ws.panes() {
                    pane.update(cx, |pane, _| {
                        pane.unpreview_item_if_preview(item_id);
                    });
                }
            });
        }
        self.sync_file_preview_flags(cx);
        cx.notify();
    }

    pub(crate) fn on_service_item_added(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        item: &dyn workspace::ItemHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let item_id = item.item_id();
        let title = item.tab_content_text(0, cx).to_string();
        let preview = hosted
            .read(cx)
            .active_pane()
            .read(cx)
            .is_active_preview_item(item_id);

        // Allocate id before borrowing panes mutably.
        let existing_id = self.panes.get(&key).and_then(|pane| {
            pane.tabs.iter().find_map(|t| match &t.content {
                TabContent::File { item_id: id, .. } if *id == item_id => Some(t.id),
                _ => None,
            })
        });
        let new_id = existing_id.unwrap_or_else(|| self.next_tab_id());
        let key_for_slot = key.clone();

        let pane = self.panes.entry(key).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        // A new preview replaces the old one *in place*. It used to drop the
        // old preview tab and append the new one, so browsing files from the
        // tree made the italic tab jump to the end of the strip each time.
        let mut replace_at = None;
        if preview {
            let is_other_preview = |t: &Tab| {
                matches!(&t.content, TabContent::File { preview: true, item_id: id } if *id != item_id)
            };
            replace_at = pane.tabs.iter().position(|t| is_other_preview(t));
            pane.tabs.retain(|t| !is_other_preview(t));
        }
        if let Some(existing) = pane.tabs.iter_mut().find(|t| {
            matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id)
        }) {
            existing.title = title;
            existing.content = TabContent::File { item_id, preview };
            pane.active = existing.id;
        } else {
            let tab = Tab {
                id: new_id,
                title,
                content: TabContent::File { item_id, preview },
                agent: None,
            };
            // The slot of a preview zed already closed (see `preview_slot`).
            let freed = self
                .preview_slot
                .take()
                .filter(|(k, _, at)| {
                    preview
                        && *k == key_for_slot
                        && at.elapsed() < std::time::Duration::from_millis(500)
                })
                .map(|(_, ix, _)| ix);
            match replace_at.or(freed) {
                Some(ix) if ix <= pane.tabs.len() => pane.tabs.insert(ix, tab),
                _ => pane.tabs.push(tab),
            }
            pane.active = new_id;
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    pub(crate) fn on_service_item_removed(
        &mut self,
        item_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let Some(pane) = self.panes.get_mut(&key) else { return };
        let was_active = pane.tabs.iter().any(|t| {
            t.id == pane.active
                && matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id)
        });
        // Any closed file tab's slot, not just a flagged preview's: zed's
        // `close_current_preview_item` clears the preview id *before* removing
        // the item, so our flag sync has already marked the tab non-preview by
        // the time this runs. The timestamp below limits reuse to a preview
        // that arrives in the same replacement.
        self.preview_slot = pane
            .tabs
            .iter()
            .position(|t| {
                matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id)
            })
            .map(|ix| (key.clone(), ix, std::time::Instant::now()));
        pane.tabs.retain(|t| {
            !matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id)
        });
        if was_active {
            pane.active = pane.tabs.last().map(|t| t.id).unwrap_or(0);
        }
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx);
        } else {
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    pub(crate) fn on_service_active_item_changed(
        &mut self,
        hosted: &Entity<::workspace::Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sync_file_preview_flags(cx);
        let Some(item) = hosted.read(cx).active_item(cx) else { return };
        let item_id = item.item_id();
        let Some(r) = self.resolve() else { return };
        let Some(pane) = self.panes.get_mut(&r.key) else { return };
        if let Some(tab) = pane.tabs.iter().find(|t| {
            matches!(&t.content, TabContent::File { item_id: id, .. } if *id == item_id)
        }) {
            if pane.active != tab.id {
                pane.active = tab.id;
                self.focus_active_pane(window, cx);
                cx.notify();
            }
        }
    }

    /// Switch bodies between unified and side-by-side.
    ///
    /// Costs nothing beyond a repaint: both layouts are derived from the same
    /// parsed rows, so the cache survives and no diff is re-fetched.
    pub(crate) fn set_diff_view(&mut self, view: DiffView, cx: &mut Context<Self>) {
        if self.prefs.diff_view == view {
            return;
        }
        self.prefs.diff_view = view;
        save_prefs_to(&self.prefs_path, &self.prefs);
        cx.notify();
    }

    /// Move the diff surface between the dock and the tab strip.
    ///
    /// Switching to `Dock` closes every open Diff tab across all live panes: a
    /// tab drawing a surface that has moved elsewhere is a tab that renders
    /// nothing, and leaving one behind would look like the setting half-worked.
    pub(crate) fn set_diff_location(
        &mut self,
        location: DiffLocation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.prefs.diff_location == location {
            return;
        }
        self.prefs.diff_location = location;
        if location == DiffLocation::Dock {
            let keys: Vec<String> = self.panes.keys().cloned().collect();
            for key in keys {
                let Some(pane) = self.panes.get_mut(&key) else { continue };
                let closing: Vec<u64> = pane
                    .tabs
                    .iter()
                    .filter(|t| matches!(t.content, TabContent::Diff))
                    .map(|t| t.id)
                    .collect();
                if closing.is_empty() {
                    continue;
                }
                pane.tabs.retain(|t| !closing.contains(&t.id));
                if closing.contains(&pane.active) {
                    // Prefer the tab to the left, the way close_tab does.
                    pane.active = pane.tabs.last().map(|t| t.id).unwrap_or(0);
                }
            }
            // A worktree left with no tabs is a dead end; give it a shell back,
            // the same guarantee close_tab makes.
            if self
                .resolve()
                .and_then(|r| self.panes.get(&r.key))
                .is_some_and(|p| p.tabs.is_empty())
            {
                self.open_tab(TabKind::Shell, window, cx);
            }
            // Moving the diff back to the dock should also show it there.
            self.prefs.diff_open = true;
            self.save_panes();
        }
        self.save_prefs(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Move a tab within its own pane set, then persist the new order.
    ///
    /// Sorts the existing tabs rather than rebuilding them, so the live
    /// entities — a pty and the process on the other end of it — travel with
    /// their tab instead of being dropped and respawned.
    pub(crate) fn move_tab(&mut self, key: &str, dragged: u64, onto: u64) {
        let Some(pane) = self.panes.get_mut(key) else { return };
        let ids: Vec<u64> = pane.tabs.iter().map(|t| t.id).collect();
        let order = reordered(&ids, dragged, onto);
        if order == ids {
            return;
        }
        pane.tabs
            .sort_by_key(|t| order.iter().position(|i| *i == t.id).unwrap_or(usize::MAX));
        self.save_panes();
    }

    /// Close a tab, dropping whatever it held — a terminal's pty and its
    /// process, a chat's `pi --mode rpc` sidecar, or a service-Workspace item.
    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let file_item = self.panes.get(&key).and_then(|p| {
            p.tabs.iter().find(|t| t.id == id).and_then(|t| match &t.content {
                TabContent::File { item_id, .. } => Some(*item_id),
                _ => None,
            })
        });
        if let Some(item_id) = file_item {
            if let Some(hosted) = self.service_workspaces.get(&key).cloned() {
                // ItemRemoved sync removes the harness tab; avoid double-remove.
                hosted.update(cx, |ws, cx| {
                    for pane in ws.panes() {
                        let has = pane.read(cx).items().any(|i| i.item_id() == item_id);
                        if has {
                            pane.update(cx, |pane, cx| {
                                pane.close_item_by_id(
                                    item_id,
                                    workspace::SaveIntent::Close,
                                    window,
                                    cx,
                                )
                                .detach_and_log_err(cx);
                            });
                            break;
                        }
                    }
                });
                return;
            }
        }
        // Is the tab being closed an agent tab? If so, killing its session ends
        // that agent, and a dead agent never writes the resolving "idle" — so its
        // pill would stay stuck on WAITING/RUNNING forever. Clear it here.
        let was_agent = self
            .panes
            .get(&key)
            .and_then(|p| p.tabs.iter().find(|t| t.id == id))
            .is_some_and(|t| t.agent.is_some());
        if self.has_tmux {
            // Explicit close tears the session down (only app-exit detaches).
            crate::tmux::kill_session(&crate::tmux::session_name(&key, id));
        }
        if was_agent {
            // Unknown (renders as IDLE) rather than Idle: we killed the process,
            // we didn't observe a clean finish. A file event can't unstick this
            // because the dead agent will never touch status.json again.
            self.conductor.stopped(&r.path);
            self.agent_status.insert(r.path.clone(), AgentState::Unknown);
        }
        let Some(pane) = self.panes.get_mut(&key) else { return };
        let Some(ix) = pane.tabs.iter().position(|t| t.id == id) else { return };
        pane.tabs.remove(ix);
        if pane.active == id {
            // Prefer the tab to the left, which is where the eye already is.
            pane.active = pane
                .tabs
                .get(ix.saturating_sub(1))
                .or_else(|| pane.tabs.last())
                .map(|t| t.id)
                .unwrap_or(0);
        }
        // A worktree with no tabs is a dead end; give it a shell back.
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx); // saves panes itself
            return;
        }
        self.focus_active_pane(window, cx);
        self.save_panes();
        cx.notify();
    }

    /// Drop the least-recently-used pane sets beyond [`MAX_LIVE_PANES`],
    /// never the one just created (M5b).
    pub(crate) fn evict_stale_panes(&mut self, keep: &str) {
        while self.panes.len() > MAX_LIVE_PANES {
            let Some(victim) = self
                .panes
                .iter()
                .filter(|(key, _)| key.as_str() != keep)
                .min_by_key(|(_, pane)| pane.last_used)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            log::info!("evicting pane {victim} (over the {MAX_LIVE_PANES}-pane cap)");
            // Dropping the PaneSet drops the terminal entity, which closes the
            // pty and terminates the agent process with it.
            self.panes.remove(&victim);
        }
    }

    pub(crate) fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if let Some(p) = self.panes.get(&r.key) {
            let Some(tab) = p.active_tab() else { return };
            let handle = match &tab.content {
                TabContent::Terminal(t) => t.read(cx).focus_handle(cx),
                TabContent::File { item_id, .. } => {
                    if let Some(hosted) = self.service_workspaces.get(&r.key) {
                        let item_id = *item_id;
                        let item = hosted.read(cx).panes().iter().find_map(|pane| {
                            pane.read(cx).items().find(|i| i.item_id() == item_id).cloned()
                        });
                        if let Some(item) = item {
                            hosted.update(cx, |ws, cx| {
                                ws.activate_item(item.as_ref(), true, true, window, cx);
                            });
                        }
                        if hosted.read(cx).active_item(cx).is_some() {
                            hosted.read(cx).focus_handle(cx)
                        } else {
                            self.editor_host_focus.clone()
                        }
                    } else {
                        self.editor_host_focus.clone()
                    }
                }
                TabContent::Chat(c) => c.read(cx).focus_handle(cx),
                TabContent::Diff => self.diff_surface_focus.clone(),
                TabContent::Editor(ws) => {
                    if ws.read(cx).active_item(cx).is_some() {
                        ws.read(cx).focus_handle(cx)
                    } else {
                        self.editor_host_focus.clone()
                    }
                }
            };
            window.focus(&handle, cx);
        }
    }

    /// Recompute the merge-base diff **off the UI thread** (M4).
    ///
    /// `compute_diff` shells out to `git diff`, which on a large repo takes
    /// long enough to visibly stall the window if run inline on selection.
    /// Results are tagged with the worktree they were computed for and the
    /// generation that requested them, so a slow diff for a worktree the user
    /// has already navigated away from is dropped rather than painted over the
    /// current one.
    /// Copy the selected diff rows to the clipboard.
    ///
    /// Copies the *code*, not the diff decoration: no gutter numbers and no
    /// +/- markers, so a pasted selection compiles.
    pub(crate) fn copy_diff_selection(&mut self, cx: &mut Context<Self>) {
        let Some((file, from, to)) = self.diff_selection.clone() else { return };
        // Reads the rows the selection was made against rather than re-parsing:
        // you cannot have selected lines in a body that was never drawn.
        let Some(rows) = self.cached_rows(&file) else { return };
        let text: Vec<String> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| *i >= from && *i <= to)
            .filter(|(_, r)| r.number.is_some())
            .map(|(_, r)| r.text.clone())
            .collect();
        if text.is_empty() {
            return;
        }
        cx.write_to_clipboard(gpui::ClipboardItem::new_string(text.join("\n")));
        self.note = format!("copied {} line(s)", text.len());
        cx.notify();
    }

    /// Begin, extend, or clear a row selection within one file's diff.
    pub(crate) fn select_diff_row(&mut self, file: &str, row: usize, extend: bool, cx: &mut Context<Self>) {
        self.diff_selection = match (self.diff_selection.take(), extend) {
            // Shift-click extends, but only within the same file — a range
            // spanning two bodies would have no meaning.
            (Some((f, from, _)), true) if f == file => {
                Some((f, from.min(row), from.max(row)))
            }
            _ => Some((file.to_string(), row, row)),
        };
        cx.notify();
    }

    pub(crate) fn extend_diff_selection(&mut self, file: &str, row: usize, cx: &mut Context<Self>) {
        if let Some((f, from, _)) = self.diff_selection.clone()
            && f == file
        {
            self.diff_selection = Some((f, from.min(row), from.max(row)));
            cx.notify();
        }
    }

    /// Open the comment editor for a diff line, seeded with any existing note.
    pub(crate) fn begin_comment(
        &mut self,
        file: String,
        line: u32,
        at: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Land on Review so the comment popover isn't drawn over Files/Git.
        if self.prefs.diff_location == DiffLocation::Dock {
            self.prefs.tool_tab = ToolTab::Review;
            self.prefs.diff_open = true;
            self.save_prefs(cx);
        }
        let existing = self
            .resolve()
            .and_then(|r| self.review.get(&r.key, &file, line))
            .map(|c| c.body.clone())
            .unwrap_or_default();
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("What should the agent change here?", window, cx);
            if !existing.is_empty() {
                e.set_text(existing.as_str(), window, cx);
            }
            e
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.comment_draft = Some(CommentDraft {
            file,
            line,
            editor,
            at,
        });
        cx.notify();
    }

    /// Enter in the comment popover (routed from `menu::Confirm`).
    pub(crate) fn confirm_comment(&mut self, _: &menu::Confirm, _window: &mut Window, cx: &mut Context<Self>) {
        self.commit_comment(cx);
    }

    /// Escape in the comment popover (routed from `menu::Cancel`): abandon it.
    pub(crate) fn cancel_comment(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        self.comment_draft = None;
        cx.notify();
    }

    pub(crate) fn commit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.comment_draft.take() else { return };
        let body = draft.editor.read(cx).text(cx);
        if let Some(r) = self.resolve() {
            self.review.set(&r.key, &draft.file, draft.line, &body);
            self.review.save(&self.review_path);
        }
        cx.notify();
    }

    /// Hand the review to the agent: copy it formatted, addressed by file and
    /// line, ready to paste into the agent's terminal or chat.
    pub(crate) fn copy_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let review = self.review.as_review(&r.key);
        if review.is_empty() {
            self.note = "no review comments yet".to_string();
        } else {
            let n = self.review.count(&r.key);
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(review));
            self.note = format!("copied {n} comment(s) — paste into the agent");
        }
        cx.notify();
    }

    /// Write the review to a markdown file next to the harness state.
    ///
    /// Not into the repo: a review is the user's working notes, and dropping an
    /// untracked file into their project is the thing the git-exclude work went
    /// out of its way to avoid.
    pub(crate) fn save_review_markdown(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let md = self.review.as_markdown(&r.key, &r.branch);
        if md.is_empty() {
            self.note = "no comments to save".to_string();
            cx.notify();
            return;
        }
        let file = crate::diff_review::ReviewNotes::markdown_path_in(&self.state_dir, &r.key);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&file, md) {
            Ok(()) => self.note = format!("saved {}", file.display()),
            Err(e) => self.note = format!("could not save review: {e}"),
        }
        cx.notify();
    }

    /// Discard every comment for this worktree, behind a two-click confirm.
    pub(crate) fn clear_review(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        if !self.clearing_review {
            self.clearing_review = true;
            self.note = "click again to discard all comments".to_string();
        } else {
            self.review.clear(&r.key);
            self.review.save(&self.review_path);
            self.clearing_review = false;
            self.note = "comments discarded".to_string();
        }
        cx.notify();
    }

    /// Toggle between the raw diff and a rendered preview of the selected
    /// markdown file.
    pub(crate) fn toggle_md_preview(&mut self, file: &str, cx: &mut Context<Self>) {
        if self.md_preview.as_ref().is_some_and(|(f, _)| f == file) {
            self.md_preview = None;
            cx.notify();
            return;
        }
        let Some(r) = self.resolve() else { return };
        let abs = if let Some(uf) = self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == file)
        {
            let Some(member) = self.manager.registry.get(&uf.member_id) else {
                self.note = format!("worktree {} not found", uf.member_id);
                cx.notify();
                return;
            };
            member.path.join(&uf.file.path)
        } else {
            r.editor_path.join(file)
        };
        self.md_preview = Some((
            file.to_string(),
            cx.new(|cx| crate::markdown_preview::MarkdownPreview::new(abs, cx)),
        ));
        cx.notify();
    }

    /// Recompute the diff for a *new* selection: this resets the review view
    /// (collapses files, drops the markdown preview) because the user has
    /// switched to a different set of changes, and retargets the live file
    /// watcher at the newly-selected root.
    pub(crate) fn recompute_diff(&mut self, cx: &mut Context<Self>) {
        self.expanded.clear();
        self.diff_focus = None;
        self.md_preview = None;
        if let Some(r) = self.resolve() {
            // Keep the live watch pointed at whatever the diff panel shows.
            self.diff_watch.watch(&r.path);
        }
        // A push offer belongs to one branch. Carrying an armed control — or a
        // count — across a selection change would let a click aimed at one
        // branch push another.
        self.push_armed = false;
        self.push_state = None;
        self.refresh_push_state(cx);
        self.spawn_diff_compute(cx);
    }

    /// Recompute the diff for the *same* selection, in response to a live file
    /// change. Deliberately does **not** clear `expanded`/`md_preview`: the user
    /// is reviewing the same worktree, and resetting their expand/preview state
    /// on every agent edit would make review impossible.
    pub(crate) fn refresh_diff_live(&mut self, cx: &mut Context<Self>) {
        self.spawn_diff_compute(cx);
    }

    /// Recompute every card's ahead/behind/dirty off the UI thread and merge the
    /// answers back in.
    ///
    /// Separate from `refresh()`, which rebuilds the whole view model — registry
    /// included — with synchronous git calls on the render thread. That is fine
    /// for a mutation the user just asked for, and unacceptable on a watcher
    /// signal: `git status` on a large tree takes long enough to drop frames,
    /// and a build can fire the watcher repeatedly. This path touches only the
    /// numbers on the badges, so the cards, the selection and the registry are
    /// left exactly as they are.
    pub(crate) fn refresh_status_live(&mut self, cx: &mut Context<Self>) {
        if self.status_pending {
            self.status_rerun = true;
            return;
        }
        self.status_rerun = false;
        // Every card, main rows included — each with the compare ref its own
        // repo resolved, so a worktree is never measured against another repo's
        // upstream.
        let mut targets: Vec<(PathBuf, String)> = Vec::new();
        for repo in &self.data.repos {
            targets.push((repo.repo_root.clone(), repo.compare_ref.clone()));
            for wt in &repo.worktrees {
                targets.push((wt.path.clone(), repo.compare_ref.clone()));
            }
        }
        if targets.is_empty() {
            return;
        }
        self.status_pending = true;

        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    targets
                        .into_iter()
                        // A failed probe is skipped, not zeroed: `git status`
                        // racing a commit's `index.lock` fails transiently, and
                        // `unwrap_or_default` turned that into a clean `~0` card
                        // until the next tick.
                        .filter_map(|(path, compare_ref)| {
                            let s = git.status(&path, Some(&compare_ref)).ok()?;
                            // The branch is re-read too. It used to come only
                            // from `refresh()`, i.e. structural changes, so a
                            // `git switch` in a worktree — or an agent renaming
                            // its branch — left the old name on the card for
                            // the rest of the session.
                            let branch = git.current_branch(&path).ok();
                            Some(LiveStatus {
                                path,
                                branch,
                                ahead: s.ahead,
                                behind: s.behind,
                                dirty: s.dirty_files,
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.status_pending = false;
                // Only re-render when a number actually moved. The backstop tick
                // runs forever on an idle app, and an unconditional notify would
                // repaint the window every few seconds for nothing.
                if apply_live_status(&mut ws.data, computed) {
                    cx.notify();
                }
                if ws.status_rerun {
                    ws.refresh_status_live(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Re-probe the tool requirements, off the UI thread.
    ///
    /// Off-thread because probing spawns up to nine processes to ask each tool
    /// its version, which is a visible stall on the render thread — and this
    /// runs from a button the user presses right after installing something.
    pub(crate) fn rerun_preflight(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |view, cx| {
            let checks = cx
                .background_spawn(async move {
                    crate::preflight::probe(crate::preflight::version_on_path)
                })
                .await;
            view.update(cx, |ws, cx| {
                // Only the result changes here. Closing on a clean re-check
                // would snatch the screen away at the exact moment it finally
                // says everything passes — the one moment worth reading. It
                // closes when the user says so, via Continue.
                ws.preflight = checks;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Close the requirements screen and remember that it has been seen, so a
    /// healthy machine is never shown it again unprompted.
    pub(crate) fn dismiss_preflight(&mut self, cx: &mut Context<Self>) {
        if crate::preflight::blocked(&self.preflight) {
            return;
        }
        self.preflight_open = false;
        if !self.prefs.seen_preflight {
            self.prefs.seen_preflight = true;
            self.save_prefs(cx);
        }
        cx.notify();
    }

    /// Probe the selected root's standing against its own tracking branch, off
    /// the UI thread.
    ///
    /// Separate from `refresh_status_live` rather than folded into it, for two
    /// reasons: it asks about one worktree (the one whose diff is on screen)
    /// where that asks about every card, and it must run on selection change,
    /// when the other pass may legitimately be mid-flight for the *previous*
    /// selection and would skip this one on its `status_pending` guard.
    pub(crate) fn refresh_push_state(&mut self, cx: &mut Context<Self>) {
        if self.push_probe_pending {
            return;
        }
        let Some(r) = self.resolve() else {
            self.push_state = None;
            return;
        };
        // A worktree's ahead/behind compares against the repo's default branch;
        // that is the right fallback for a branch with no upstream, because it
        // is what publishing would send.
        let fallback = self
            .data
            .repos
            .iter()
            .find(|repo| r.path.starts_with(&repo.repo_root) || repo.repo_root == r.path)
            .map(|repo| repo.compare_ref.clone())
            .unwrap_or_else(|| "origin/HEAD".to_string());
        let path = r.path.clone();
        self.push_probe_pending = true;

        cx.spawn(async move |view, cx| {
            let probed = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    let state = git.push_state(&path, &fallback);
                    (path, state)
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.push_probe_pending = false;
                // Drop an answer for a selection the user has already left.
                log::debug!(
                    "push state {}: upstream={:?} unpushed={}",
                    probed.0.display(),
                    probed.1.upstream,
                    probed.1.unpushed
                );
                if ws.resolve().is_some_and(|r| r.path == probed.0) && ws.push_state.as_ref() != Some(&probed) {
                    ws.push_state = Some(probed);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The push control's two steps. The first click arms it — the label then
    /// states the remote and the count, because "push" alone does not say where
    /// — and the second runs git.
    ///
    /// A branch with no upstream is *published*: `push -u <remote> <branch>`,
    /// which creates the remote branch and records the tracking ref so the next
    /// push is an ordinary one.
    pub(crate) fn push_branch(&mut self, cx: &mut Context<Self>) {
        if self.pushing {
            return;
        }
        let Some((_, state)) = self.push_state.clone() else { return };
        let Some(r) = self.resolve() else { return };
        if state.unpushed == 0 {
            return;
        }
        if !self.push_armed {
            self.push_armed = true;
            cx.notify();
            return;
        }

        // Publishing needs a remote to publish to; without one there is nothing
        // honest to do, and git's own error would arrive seconds later.
        let publish = match &state.upstream {
            Some(_) => None,
            None => match GitWorktrees::new(SystemRunner).default_remote(&r.path) {
                Some(remote) => Some((remote, r.branch.clone())),
                None => {
                    self.push_armed = false;
                    self.note = "no remote to publish to".to_string();
                    cx.notify();
                    return;
                }
            },
        };

        self.push_armed = false;
        self.pushing = true;
        self.note = String::new();
        cx.notify();

        let (path, count) = (r.path.clone(), state.unpushed);
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move {
                    let git = GitWorktrees::new(SystemRunner);
                    let publish = publish.as_ref().map(|(r, b)| (r.as_str(), b.as_str()));
                    git.push(&path, publish)
                })
                .await;
            view.update(cx, |ws, cx| {
                ws.pushing = false;
                ws.note = match result {
                    // git says what it did on stderr; its own words beat ours,
                    // and "Everything up-to-date" is worth seeing verbatim.
                    Ok(out) => {
                        let last = out.lines().last().unwrap_or("").trim().to_string();
                        if last.is_empty() {
                            format!("pushed {count} commit{}", if count == 1 { "" } else { "s" })
                        } else {
                            last
                        }
                    }
                    Err(e) => format!("push failed: {e}"),
                };
                // Whatever happened, the counts on screen are now stale.
                ws.refresh_push_state(cx);
                ws.refresh_status_live(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Spawn the off-thread git diff, discarding a result whose selection the
    /// user has since left (the `diff_gen` staleness guard).
    pub(crate) fn spawn_diff_compute(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else {
            self.diff = worktrees::Diff::default();
            self.unified_diff = UnifiedDiff::default();
            self.row_cache.borrow_mut().clear();
            // Bumped here too, not only on the path that spawns: a per-file
            // body fetch may be in flight, and clearing the cache without
            // invalidating it would let that fetch land afterwards and
            // repopulate rows for a selection that no longer exists.
            self.diff_gen = self.diff_gen.wrapping_add(1);
            self.diff_pending = false;
            cx.notify();
            return;
        };

        self.diff_gen = self.diff_gen.wrapping_add(1);
        let generation = self.diff_gen;
        self.diff_pending = true;

        // Copied out before the closures below: they run on a background
        // thread and cannot borrow `self`.
        let diff_base = self.prefs.diff_base;
        if let Some(task_id) = r.task_id.clone() {
            let members: Vec<(String, String, PathBuf, String)> = self
                .task_store
                .get(&task_id)
                .map(|t| {
                    t.members
                        .iter()
                        .map(|m| {
                            let compare = self
                                .data
                                .repos
                                .iter()
                                .find(|r| r.name == m.repo)
                                .map(|r| r.compare_ref.clone())
                                .unwrap_or_else(|| "HEAD".into());
                            // Per member, so a multi-repo task honours the
                            // setting in every one of its repos.
                            let compare = diff_base_ref(diff_base, &compare).to_string();
                            (m.worktree_id.clone(), m.repo.clone(), m.path.clone(), compare)
                        })
                        .collect()
                })
                .unwrap_or_default();
            cx.spawn(async move |view, cx| {
                let computed = cx
                    .background_spawn(async move {
                        UnifiedDiff::from_members(
                            &GitWorktrees::new(SystemRunner),
                            &members,
                        )
                    })
                    .await;
                let _ = view.update(cx, |ws, cx| {
                    if ws.diff_gen != generation {
                        return;
                    }
                    // Flatten into Diff for existing panel machinery. Keys stay
                    // `repo/path` so multi-repo files remain distinct; the panel
                    // groups by repo and shows the relative path under each.
                    let mut files = Vec::new();
                    for f in &computed.files {
                        files.push(worktrees::DiffFile {
                            path: format!("{}/{}", f.repo, f.file.path),
                            change: f.file.change,
                            adds: f.file.adds,
                            dels: f.file.dels,
                            untracked: f.file.untracked,
                            renamed_from: f.file.renamed_from.clone(),
                        });
                    }
                    // `base` stays empty here: a unified diff spans members
                    // with different merge-bases, so there is no single one.
                    // Per-file fetches read the base off `unified_diff`, which
                    // records each member's own.
                    ws.diff = worktrees::Diff {
                        base: String::new(),
                        files,
                        untracked_elided: computed.untracked_elided,
                    };
                    ws.unified_diff = computed;
                    ws.row_cache.borrow_mut().clear();
                    ws.diff_pending = false;
                    cx.notify();
                });
            })
            .detach();
            return;
        }

        let (path, compare_ref) = (
            r.path.clone(),
            diff_base_ref(diff_base, &r.compare_ref).to_string(),
        );
        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    compute_diff(&GitWorktrees::new(SystemRunner), &path, &compare_ref)
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                if ws.diff_gen != generation {
                    return;
                }
                ws.diff = computed;
                ws.unified_diff = UnifiedDiff::default();
                // The parsed rows describe the diff that just went away.
                ws.row_cache.borrow_mut().clear();
                ws.diff_pending = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Kill every tmux session backing a worktree's tabs and drop its persisted
    /// record. Used when a worktree is deleted or archived, so agents don't leak
    /// as detached sessions and a later same-named worktree can't reattach to
    /// the old one. Reads the PERSISTED records (not just live panes) so evicted
    /// tabs' sessions are torn down too.
    pub(crate) fn teardown_sessions(&mut self, id: &str) {
        if self.has_tmux {
            let names: Vec<String> = self
                .panes_state
                .worktrees
                .get(id)
                .map(|wp| wp.tabs.iter().map(|t| crate::tmux::session_name(id, t.id)).collect())
                .unwrap_or_default();
            for name in names {
                crate::tmux::kill_session(&name);
            }
        }
        self.panes_state.worktrees.remove(id);
        crate::panes_state::save(&self.panes_path, &self.panes_state);
    }

    /// Draw the next globally-unique, persisted tab id.
    pub(crate) fn next_tab_id(&mut self) -> u64 {
        // next_tab_id starts at 0 on a fresh install; first id handed out is 1.
        self.panes_state.next_tab_id += 1;
        let id = self.panes_state.next_tab_id;
        crate::panes_state::save(&self.panes_path, &self.panes_state);
        id
    }

    /// Rebuild the persisted tab records for the CURRENTLY-LIVE pane sets and
    /// write them. Worktrees not in `self.panes` right now (evicted, or not yet
    /// restored this session) keep their existing records, so lazy
    /// restore-on-reselect still finds them — eviction stays non-destructive.
    pub(crate) fn save_panes(&mut self) {
        use crate::panes_state::{TabRecord, WorktreePanes};
        // Collect first to avoid borrowing self.panes and self.panes_state at once.
        let updates: Vec<(String, WorktreePanes)> = self
            .panes
            .iter()
            .map(|(key, pane)| {
                let tabs: Vec<TabRecord> = pane
                    .tabs
                    .iter()
                    .filter(|t| !matches!(t.content, TabContent::File { .. }))
                    .map(|t| TabRecord {
                        id: t.id,
                        kind: t.kind_tag(),
                        title: t.title.clone(),
                    })
                    .collect();
                let active = pane
                    .tabs
                    .iter()
                    .find(|t| t.id == pane.active && !matches!(t.content, TabContent::File { .. }))
                    .map(|t| t.id)
                    .or_else(|| tabs.first().map(|t| t.id));
                (key.clone(), WorktreePanes { active, tabs })
            })
            .collect();
        for (key, wp) in updates {
            self.panes_state.worktrees.insert(key, wp);
        }
        crate::panes_state::save(&self.panes_path, &self.panes_state);
    }

    /// Recreate a worktree's persisted tabs on first build. Live tmux sessions
    /// reattach; dead ones (reboot / killed) come back as a plain shell — never
    /// an unattended agent relaunch. Returns false if there was nothing to
    /// restore.
    fn restore_tabs(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use crate::panes_state::TabRecordKind as K;
        let Some(saved) = self.panes_state.worktrees.get(key) else { return false };
        if saved.tabs.is_empty() {
            return false;
        }
        // Clone out before mutating self.
        let records: Vec<(u64, K, String)> = saved
            .tabs
            .iter()
            .map(|t| (t.id, t.kind.clone(), t.title.clone()))
            .collect();
        let saved_active = saved.active;
        let live = if self.has_tmux { crate::tmux::live_sessions() } else { Vec::new() };

        for (id, kind, title) in records {
            match kind {
                K::Editor => self.open_tab(TabKind::Editor, window, cx),
                K::Chat => self.open_tab(TabKind::Chat, window, cx),
                K::Diff => self.open_tab(TabKind::Diff, window, cx),
                K::Shell => self.restore_terminal_tab(key, id, None, &title, &live, window, cx),
                K::Agent(a) => self.restore_terminal_tab(key, id, Some(a), &title, &live, window, cx),
            }
        }

        // Restore the active tab if its id survived (ids are stable for
        // terminal/agent tabs; editor/chat drew fresh ids so may not match).
        if let Some(active) = saved_active
            && let Some(pane) = self.panes.get_mut(key)
            && pane.tabs.iter().any(|t| t.id == active)
        {
            pane.active = active;
            self.focus_active_pane(window, cx);
        }
        let restored = self.panes.contains_key(key);
        if restored {
            self.save_panes();
        }
        restored
    }

    /// Rebuild one terminal/agent tab, preserving its persisted id. Live session
    /// → reattach (`attach_only`); dead → plain shell in the worktree.
    fn restore_terminal_tab(
        &mut self,
        key: &str,
        id: u64,
        agent: Option<AgentKind>,
        title: &str,
        live: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let name = crate::tmux::session_name(key, id);
        let alive = self.has_tmux && crate::tmux::is_live(&name, live);
        // Keep the agent mark only when we actually reattached a live agent
        // session; a dead agent comes back as a plain shell.
        let mark = if alive { agent } else { None };
        let session = self
            .has_tmux
            .then(|| crate::terminal_pane::TerminalSession {
                name,
                attach_only: alive,
                // Ignored when reattaching; a dead session is recreated here and
                // should come back with the same environment a fresh tab gets.
                env: self.pane_env(&r),
            });
        // Restore never passes a command: a live session is reattached
        // (attach_only ignores it), a dead one is a plain shell.
        let content = TabContent::Terminal(cx.new(|cx| {
            crate::terminal_pane::TerminalPane::new(r.path.clone(), None, session, window, cx)
        }));

        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self.panes.entry(key.to_string()).or_insert_with(|| PaneSet {
            tabs: Vec::new(),
            active: 0,
            last_used: 0,
        });
        pane.last_used = self.pane_clock;
        pane.tabs.push(Tab { id, title: title.to_string(), content, agent: mark });
        pane.active = id;
    }
}

/// The `pid` an agent stamped into `<root>/.harness/status.json`, if present.
/// `None` when the file is missing/malformed or the writer didn't record one —
/// callers treat that as "can't verify" and leave the pill alone.
fn status_pid(root: &Path) -> Option<u32> {
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
fn pid_alive(pid: u32) -> bool {
    pid != 0 && unsafe { libc::kill(pid as libc::pid_t, 0) } == 0
}

/// A `pid ppid comm` snapshot of every process, for agent-liveness checks.
/// Empty on any error — callers treat that as "no evidence" and don't act on it.
fn ps_snapshot() -> String {
    std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// One card's freshly probed git state, off the UI thread.
#[derive(Debug, Clone)]
pub(crate) struct LiveStatus {
    pub path: PathBuf,
    /// `None` when the branch probe failed; the card keeps the name it had.
    pub branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub dirty: usize,
}

/// Merge recomputed status into the cards, keyed by path. Returns whether
/// anything changed.
///
/// A path that no longer matches a card is dropped: the worktree was deleted
/// while its status was in flight, and `refresh()` has already rebuilt the
/// list without it.
pub(crate) fn apply_live_status(data: &mut crate::data::WorkspaceData, computed: Vec<LiveStatus>) -> bool {
    fn set<T: PartialEq>(slot: &mut T, value: T, changed: &mut bool) {
        if *slot != value {
            *slot = value;
            *changed = true;
        }
    }
    let mut changed = false;
    for st in computed {
        log::trace!("status {}: {:?} ↑{} ↓{} ~{}", st.path.display(), st.branch, st.ahead, st.behind, st.dirty);
        for repo in &mut data.repos {
            if repo.repo_root == st.path {
                set(&mut repo.ahead, st.ahead, &mut changed);
                set(&mut repo.behind, st.behind, &mut changed);
                set(&mut repo.dirty, st.dirty, &mut changed);
                if let Some(b) = &st.branch {
                    set(&mut repo.head_branch, b.clone(), &mut changed);
                }
            }
            for wt in &mut repo.worktrees {
                if wt.path == st.path
                    && let Some(b) = &st.branch
                {
                    set(&mut wt.branch, b.clone(), &mut changed);
                }
            }
        }
        for task in &mut data.tasks {
            for m in &mut task.members {
                if m.path == st.path {
                    set(&mut m.ahead, st.ahead, &mut changed);
                    set(&mut m.behind, st.behind, &mut changed);
                    set(&mut m.dirty, st.dirty, &mut changed);
                    if let Some(b) = &st.branch {
                        set(&mut m.branch, b.clone(), &mut changed);
                    }
                }
            }
            task.dirty = task.members.iter().map(|m| m.dirty).sum();
        }
    }
    changed
}

#[cfg(test)]
mod live_status_tests {
    use super::{apply_live_status, LiveStatus};
    use crate::data::{RepoView, TaskMemberView, TaskView, WorkspaceData, WorktreeView};
    use std::path::PathBuf;

    fn data() -> WorkspaceData {
        WorkspaceData {
            repos: vec![RepoView {
                name: "app".into(),
                repo_root: PathBuf::from("/r/app"),
                default_branch: "main".into(),
                head_branch: "main".into(),
                compare_ref: "origin/main".into(),
                ahead: 0,
                behind: 0,
                dirty: 0,
                worktrees: vec![WorktreeView {
                    id: "wt1".into(),
                    name: "wt1".into(),
                    branch: "feat/old".into(),
                    path: PathBuf::from("/r/app-wt1"),
                    task_id: Some("t1".into()),
                }],
            }],
            tasks: vec![TaskView {
                id: "t1".into(),
                name: "t".into(),
                initials: "T".into(),
                color: 0,
                members: vec![TaskMemberView {
                    worktree_id: "wt1".into(),
                    repo: "app".into(),
                    branch: "feat/old".into(),
                    path: PathBuf::from("/r/app-wt1"),
                    ahead: 0,
                    behind: 0,
                    dirty: 0,
                    is_home: true,
                }],
                dirty: 0,
            }],
        }
    }

    fn st(path: &str, branch: Option<&str>, dirty: usize) -> LiveStatus {
        LiveStatus { path: PathBuf::from(path), branch: branch.map(Into::into), ahead: 1, behind: 0, dirty }
    }

    #[test]
    fn a_branch_switch_reaches_every_card_that_shows_it() {
        let mut d = data();
        assert!(apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2), st("/r/app", Some("dev"), 0)]));
        assert_eq!(d.repos[0].head_branch, "dev");
        assert_eq!(d.repos[0].worktrees[0].branch, "feat/new");
        assert_eq!(d.tasks[0].members[0].branch, "feat/new");
        assert_eq!(d.tasks[0].members[0].dirty, 2);
        assert_eq!(d.tasks[0].dirty, 2);
    }

    #[test]
    fn a_failed_branch_probe_keeps_the_known_name() {
        let mut d = data();
        apply_live_status(&mut d, vec![st("/r/app-wt1", None, 0)]);
        assert_eq!(d.tasks[0].members[0].branch, "feat/old");
    }

    #[test]
    fn an_unchanged_answer_reports_no_change() {
        let mut d = data();
        apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2)]);
        assert!(!apply_live_status(&mut d, vec![st("/r/app-wt1", Some("feat/new"), 2)]));
    }
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
