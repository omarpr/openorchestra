//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

mod alerts;
mod git_status;
mod hosted;
mod review;
mod tabs;
pub(crate) use alerts::*;

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

}

