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

/// The main checkout's selection.
///
/// `compare_ref` is the repo's default branch, like a worktree's. It used to
/// be a hardcoded `"HEAD"` from before the diff-base menu existed (main only
/// ever showed uncommitted work); with the menu, that made "vs <branch>" read
/// "vs HEAD" and diff exactly what "Uncommitted" does. Uncommitted still maps
/// to `HEAD` through [`diff_base_ref`].
fn main_resolved(r: &crate::data::RepoView) -> Resolved {
    Resolved {
        key: format!("main:{}", r.name),
        path: r.repo_root.clone(),
        editor_path: r.repo_root.clone(),
        agent_kind: None,
        compare_ref: r.compare_ref.clone(),
        branch: r.head_branch.clone(),
        task_id: None,
    }
}

impl HarnessWorkspace {
    pub(crate) fn resolve(&self) -> Option<Resolved> {
        let primary = self.data.repos.first()?;
        let main_of = main_resolved;
        match &self.selection {
            Selection::Main(repo) => {
                let r = self
                    .data
                    .repos
                    .iter()
                    .find(|x| &x.name == repo)
                    .unwrap_or(primary);
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
            Selection::Task { task_id, member_id } => {
                self.resolve_task(task_id, member_id.as_deref())
            }
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
        use super::coordinator::{TaskFocus, task_focus};
        let task = self.task_store.get(task_id)?;
        let home = task.home_member()?;
        let compare_ref_of = |repo: &str| {
            self.data
                .repos
                .iter()
                .find(|r| r.name == repo)
                .map(|r| r.compare_ref.clone())
                .unwrap_or_else(|| "HEAD".into())
        };
        // A member id that no longer names a member (stale selection) falls
        // back to the task itself rather than blanking the pane.
        let focus = task_focus(task, member_id).or_else(|| task_focus(task, None))?;
        let focused = match focus {
            TaskFocus::Member(m) => m,
            // The coordinator has no repo: its cwd is the task folder. Editor
            // and diff baseline borrow the home member's so the editor and
            // DIFF still have a project to show.
            TaskFocus::Coordinator => {
                let tasks_root = self.data_dir.join("tasks");
                return Some(Resolved {
                    key: a2a_broker::coordinator_pane_key(task_id),
                    path: worktrees::task_coordinator_dir(&tasks_root, &task.slug),
                    editor_path: home.path.clone(),
                    agent_kind: Some(task.agent_kind.clone()),
                    compare_ref: compare_ref_of(&home.repo),
                    branch: home.branch.clone(),
                    task_id: Some(task_id.to_string()),
                });
            }
        };
        let compare_ref = compare_ref_of(&focused.repo);
        // Per-member context: agent/shell cwd and pane key follow the focused
        // member. Unified DIFF stays task-scoped.
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
        let tasks_root = self.data_dir.join("tasks");
        if r.key == a2a_broker::coordinator_pane_key(task_id) {
            // The coordinator owns no repo, so no HARNESS_WORKTREE /
            // REPO_ROOT / BRANCH: handing it the home member's would make
            // hooks and the agent treat the home repo as its own again.
            let mut pairs = vec![
                ("HARNESS_TASK_ID".to_string(), task_id.clone()),
                ("HARNESS_TASK_SLUG".to_string(), task.slug.clone()),
            ];
            if let Some(home) = task.home_path() {
                pairs.push((
                    "HARNESS_HOME_WORKTREE".into(),
                    home.to_string_lossy().into_owned(),
                ));
            }
            if let Ok(Some(path)) = worktrees::ensure_task_agent_context(task, &tasks_root) {
                pairs.push((
                    agent_runtime::HARNESS_TASK_CONTEXT.to_string(),
                    path.to_string_lossy().into_owned(),
                ));
            }
            if let Some(info) = self.task_brokers.get(task_id) {
                pairs.push(("HARNESS_A2A_URL".into(), info.base_url.clone()));
                pairs.push(("HARNESS_A2A_KEY".into(), info.api_key.clone()));
                pairs.push((
                    "HARNESS_A2A_TENANT".into(),
                    a2a_broker::COORDINATOR_TENANT.to_string(),
                ));
            }
            return pairs;
        }
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
            pairs.push(("HARNESS_A2A_TENANT".into(), member.worktree_id.clone()));
        }
        pairs
    }

    pub(crate) fn ensure_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        // Every path that can spawn a task pane (select, startup restore,
        // notification click) goes through here, so start the broker first:
        // a coordinator spawned before it would bake a tmux env with no
        // HARNESS_A2A_* and nothing would put them back. Idempotent, and a
        // no-op for single-member tasks.
        if let Some(tid) = &r.task_id {
            self.ensure_task_broker(tid);
        }
        self.pane_clock = self.pane_clock.wrapping_add(1);
        if let Some(existing) = self.panes.get_mut(&r.key) {
            existing.last_used = self.pane_clock;
            return;
        }

        // The coordinator's cwd is the task folder, which nothing else
        // creates: make it before a shell, agent, or status watch lands there.
        let is_coordinator = r
            .task_id
            .as_ref()
            .is_some_and(|tid| r.key == a2a_broker::coordinator_pane_key(tid));
        if is_coordinator && let Err(e) = std::fs::create_dir_all(&r.path) {
            log::warn!("could not create coordinator dir {}: {e}", r.path.display());
        }

        let restored = self.restore_tabs(&r.key, window, cx);
        if !restored {
            // A multi-project task opens on its coordinator agent, so
            // clicking the task lands somewhere you can talk to. Repo
            // members are specialists and open on a shell.
            match r.agent_kind.clone().filter(|_| is_coordinator) {
                Some(kind) => self.open_tab(TabKind::Agent(kind), window, cx),
                None => self.open_tab(TabKind::Shell, window, cx),
            }
        }
        self.evict_stale_panes(&r.key, cx);
    }

    /// Fidelity for notifications: prefs signal profile for custom agents,
    /// else the kind's built-in declaration.
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

    /// Prepare the worktree's agent adapter synchronously (in-memory wiring
    /// only) and return the filesystem-dependent command preparation to run
    /// on a background worker.
    ///
    /// Wires the tab's status signal and prepares the worktree's git excludes.
    ///
    /// A user command override replaces the launch command but NOT the
    /// adapter wiring: hooks, excludes, and the status watch still belong to
    /// the kind, or an overridden Claude Code would silently lose its status
    /// pill and notifications.
    pub(crate) fn prepare_agent_launch(
        &mut self,
        kind: &AgentKind,
        r: &Resolved,
        tab_id: u64,
    ) -> AgentLaunchPrep {
        let agent_pref = self.prefs.agents.get(kind.as_str());
        let signal = agent_pref.and_then(|p| p.signal.clone());
        let override_cmd = agent_pref.and_then(|p| p.command.clone());
        let runtime =
            agent_runtime::runtime_for_with_signal(kind, &r.path, &self.state_dir, signal);
        // Worktrees and main checkouts are watched up front with their own
        // kinds; only the coordinator's task folder (created on first open)
        // is watched here, so opening an agent in a worktree never retags
        // that root's fallback kind. Already watched is a harmless error.
        let is_coordinator = r
            .task_id
            .as_ref()
            .is_some_and(|tid| r.key == a2a_broker::coordinator_pane_key(tid));
        if is_coordinator && let Some(source) = &mut self.status_source {
            let _ = source.watch(&r.path, kind.clone());
        }
        // Adapters that stream their own signals (OpenCode 1.x's SSE) publish
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
        AgentLaunchPrep {
            tab_id,
            kind: kind.clone(),
            path: r.path.clone(),
            runtime,
            task: r
                .task_id
                .as_ref()
                .and_then(|tid| self.task_store.get(tid).cloned()),
            tasks_root: self.data_dir.join("tasks"),
            override_cmd,
            continue_next_agent: self.continue_next_agent,
        }
    }
}

/// Captured UI-thread inputs for preparing an agent command on a worker thread.
pub(crate) struct AgentLaunchPrep {
    tab_id: u64,
    kind: AgentKind,
    path: PathBuf,
    runtime: Box<dyn agent_runtime::AgentRuntime>,
    task: Option<worktrees::Task>,
    tasks_root: PathBuf,
    override_cmd: Option<String>,
    continue_next_agent: bool,
}

impl AgentLaunchPrep {
    /// Run filesystem-dependent setup (`on_spawn`, local git excludes, task
    /// context, `$PATH` lookup, and `prepare_launch`) off the UI thread.
    ///
    /// A missing CLI still falls back to a shell for an ordinary agent tab. A
    /// review follow-up does not: the report would never be applied, and the
    /// caller reports that the CLI is unavailable instead.
    pub(crate) fn resolve(self, review: Option<ReviewFollowup>) -> ResolvedLaunch {
        let kind_label = super::agents::agent_display(&self.kind);
        if let Err(e) = self.runtime.on_spawn(&self.path) {
            log::warn!(
                "{} setup failed for {}: {e}",
                self.kind,
                self.path.display()
            );
        }
        // The harness runs inside the user's own repository, so anything it
        // drops there would otherwise show up as uncommitted changes in their
        // project. Exclude locally rather than touching their .gitignore.
        // A coordinator's task folder is not a checkout (`.git` is a dir in
        // a main checkout, a file in a linked worktree; absent here).
        if self.path.join(".git").exists()
            && let Err(e) = GitWorktrees::new(SystemRunner)
                .exclude_locally(&self.path, self.runtime.ignored_paths())
        {
            log::warn!(
                "could not add local git excludes for {}: {e}",
                self.path.display()
            );
        }
        // Resolve the adapter command first so a prefs override can keep the
        // managed env (HARNESS_MANAGED / HARNESS_AGENT) that hooks need.
        let mut runtime_cmd = self.runtime.command(&self.path);
        let tab_env = [
            ("HARNESS_AGENT_TAB_ID".to_string(), self.tab_id.to_string()),
            (
                "HARNESS_AGENT_WORKTREE".to_string(),
                self.path.to_string_lossy().into_owned(),
            ),
        ];
        if let Some(cmd) = runtime_cmd.as_mut() {
            cmd.env.extend(tab_env.clone());
        }
        let runtime_env = runtime_cmd
            .as_ref()
            .map(|c| c.env.clone())
            .unwrap_or_else(|| Vec::from(tab_env));

        let context_path = self.task.as_ref().and_then(|task| {
            worktrees::ensure_task_agent_context(task, &self.tasks_root)
                .ok()
                .flatten()
        });

        // Review follow-ups use the same installed CLI and model as the
        // report. Free-form interactive overrides may select another model
        // or consume the initial prompt themselves.
        if let Some(cmd) = self.override_cmd.filter(|_| review.is_none()) {
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
            return ResolvedLaunch {
                command: Some(self.runtime.prepare_launch(agent_runtime::AgentCommand {
                    program: shell.into(),
                    args,
                    env,
                    label: self.kind.as_str().to_string(),
                })),
                review_cli_missing: false,
                kind_label,
            };
        }
        let review_cli_missing = review.is_some() && runtime_cmd.is_none();
        if review_cli_missing {
            log::info!(
                "{} not on PATH — the review follow-up was not opened",
                self.kind
            );
        } else if runtime_cmd.is_none() {
            log::info!("{} not on PATH — falling back to a plain shell", self.kind);
        }
        if let (Some(cmd), Some(path)) = (runtime_cmd.as_mut(), context_path.as_ref()) {
            agent_runtime::apply_task_context(cmd, &self.kind, path);
        }
        // A restart of a missing agent continues its last conversation in this
        // folder, where the CLI can scope that to the folder (see
        // `continue_args`). User overrides above are free-form lines and start
        // fresh.
        if review.is_none()
            && self.continue_next_agent
            && let Some(cmd) = runtime_cmd.as_mut()
            && let Some(args) = agent_runtime::continue_args(&self.kind, &cmd.args)
        {
            cmd.args = args;
        }
        let command = runtime_cmd.map(|cmd| {
            let mut cmd = self.runtime.prepare_launch(cmd);
            if let Some(review) = review {
                review.apply(&mut cmd);
            }
            cmd
        });
        ResolvedLaunch {
            command,
            review_cli_missing,
            kind_label,
        }
    }
}

/// What [`AgentLaunchPrep::resolve`] decided to run.
pub(crate) struct ResolvedLaunch {
    pub(crate) command: Option<agent_runtime::AgentCommand>,
    /// The follow-up CLI is not installed. The caller must not open a shell.
    pub(crate) review_cli_missing: bool,
    pub(crate) kind_label: String,
}

#[cfg(test)]
mod main_resolve_tests {
    use super::{diff_base_ref, main_resolved};
    use crate::data::RepoView;
    use crate::workspace::DiffBase;
    use std::path::PathBuf;

    fn repo(head: &str) -> RepoView {
        RepoView {
            name: "app".into(),
            repo_root: PathBuf::from("/r/app"),
            head_branch: head.into(),
            compare_ref: "origin/master".into(),
            ahead: 0,
            behind: 0,
            dirty: 0,
            worktrees: vec![],
        }
    }

    #[test]
    fn main_checkout_compares_against_the_default_branch() {
        // A main checkout on a feature branch: "vs <default>" must measure
        // the branch, not collapse to HEAD (which is just Uncommitted again).
        let r = main_resolved(&repo("feat/ports-panel"));
        assert_eq!(r.compare_ref, "origin/master");
        assert_eq!(
            DiffBase::DefaultBranch.label(&r.compare_ref),
            "vs origin/master"
        );
        assert_ne!(
            diff_base_ref(DiffBase::DefaultBranch, &r.compare_ref),
            diff_base_ref(DiffBase::Uncommitted, &r.compare_ref),
            "the two menu choices must differ"
        );
    }
}

#[cfg(test)]
mod launch_prep_tests {
    use super::*;
    use std::path::{Path, PathBuf};

    struct MissingCli;

    impl agent_runtime::AgentRuntime for MissingCli {
        fn kind(&self) -> AgentKind {
            AgentKind::Codex
        }
        fn fidelity(&self) -> agent_runtime::WaitFidelity {
            agent_runtime::WaitFidelity::Exact
        }
        fn signal_kind(&self) -> agent_runtime::SignalKind {
            agent_runtime::SignalKind::None
        }
        fn on_spawn(&self, _: &Path) -> std::io::Result<()> {
            Ok(())
        }
        fn command(&self, _: &Path) -> Option<agent_runtime::AgentCommand> {
            None
        }
    }

    fn prep() -> AgentLaunchPrep {
        AgentLaunchPrep {
            tab_id: 3,
            kind: AgentKind::Codex,
            path: PathBuf::from("/tmp/openorchestra-missing-cli"),
            runtime: Box::new(MissingCli),
            task: None,
            tasks_root: PathBuf::from("/tmp"),
            override_cmd: None,
            continue_next_agent: false,
        }
    }

    #[test]
    fn a_missing_review_cli_is_not_a_shell() {
        let review = ReviewFollowup::new(&ReviewPreferences::default(), "report", &[], &[]);
        let resolved = prep().resolve(Some(review));
        assert!(resolved.review_cli_missing);
        assert!(resolved.command.is_none());
        assert_eq!(resolved.kind_label, "Codex");
    }

    #[test]
    fn a_missing_cli_without_a_review_still_falls_back_to_a_shell() {
        let resolved = prep().resolve(None);
        assert!(!resolved.review_cli_missing);
        assert!(resolved.command.is_none());
    }
}
