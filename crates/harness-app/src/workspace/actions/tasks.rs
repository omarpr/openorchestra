//! The new-task dialog, creating a task's worktrees, and running their
//! setup hooks visibly in a shell tab.

use super::*;

impl HarnessWorkspace {
    /// Include or exclude a project in the pending new-task membership.
    /// Keeps at least one member; demoting home promotes the first remaining.
    pub(crate) fn toggle_new_task_repo(&mut self, name: &str, cx: &mut Context<Self>) {
        if let Some(pos) = self.new_task_repos.iter().position(|n| n == name) {
            if self.new_task_repos.len() <= 1 {
                // Cannot drop the last included project.
                return;
            }
            self.new_task_repos.remove(pos);
            if self.new_task_home == name {
                self.new_task_home = self.new_task_repos[0].clone();
            }
        } else {
            self.new_task_repos.push(name.to_string());
            if self.new_task_repos.len() == 1 {
                self.new_task_home = name.to_string();
            }
        }
        // Two or more: no Home radio is shown (the coordinator leads), so
        // record the first checked repo rather than a stale pick.
        if self.new_task_repos.len() >= 2 {
            self.new_task_home = self.new_task_repos[0].clone();
        }
        self.new_target_repo = self.new_task_home.clone();
        cx.notify();
    }

    /// Mark an included project as home for the pending new task.
    pub(crate) fn set_new_task_home(&mut self, name: &str, cx: &mut Context<Self>) {
        if !self.new_task_repos.iter().any(|n| n == name) {
            return;
        }
        self.new_task_home = name.to_string();
        self.new_target_repo = self.new_task_home.clone();
        cx.notify();
    }

    /// Open the new-task dialog with only `repo` included and as home — the
    /// PROJECTS row's `+`. A one-member task is how a plain worktree is made:
    /// same branch, agent and setup-hook path as any task.
    pub(crate) fn open_new_task_for_repo(
        &mut self,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.data.repos.iter().any(|r| r.name == repo) {
            return;
        }
        self.open_new_task(window, cx);
        if let Some(dialog) = self.new_task.as_mut() {
            dialog.worktree_for = Some(repo.to_string());
            dialog.name.update(cx, |e, cx| {
                e.set_placeholder_text("worktree name…", window, cx)
            });
            dialog.brief.update(cx, |e, cx| {
                e.set_placeholder_text("What should the agent work on?", window, cx)
            });
        }
    }

    /// Open the centered new-task dialog (TASKS `+`).
    pub(crate) fn open_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.data.repos.is_empty() {
            self.set_note("add a project first".to_string());
            cx.notify();
            return;
        }
        if self.new_task_repos.is_empty()
            && let Some(r) = self.data.repos.first()
        {
            self.new_task_repos = vec![r.name.clone()];
            self.new_task_home = r.name.clone();
            self.new_target_repo = r.name.clone();
        }
        let name = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text("effort name…", window, cx);
            e
        });
        // Re-render when the name changes so the branch placeholder
        // (feat/{slug}) stays in sync.
        let _name_sub = cx.subscribe(&name, |_ws, _editor, event, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                cx.notify();
            }
        });
        let brief = cx.new(|cx| {
            let mut e = Editor::auto_height(3, 8, window, cx);
            e.set_placeholder_text("What should the agents work on?", window, cx);
            e
        });
        let auto = "feat/task".to_string();
        let branch = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text(auto.as_str(), window, cx);
            e
        });
        window.focus(&name.focus_handle(cx), cx);
        self.new_task = Some(NewTaskDialog {
            name,
            brief,
            branch,
            auto_branch_shown: auto,
            _name_sub,
            worktree_for: None,
        });
        self.edit = None;
        self.menu = None;
        cx.notify();
    }

    pub(crate) fn close_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_task = None;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    pub(crate) fn submit_new_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.new_task.as_ref() else {
            return;
        };
        let name = dialog.name.read(cx).text(cx);
        let name = name.trim().to_string();
        let worktree_for = dialog.worktree_for.clone();
        if name.is_empty() {
            self.set_note(
                if worktree_for.is_some() {
                    "enter a worktree name"
                } else {
                    "enter an effort name"
                }
                .to_string(),
            );
            cx.notify();
            return;
        }
        let brief = dialog.brief.read(cx).text(cx);
        let brief = brief.trim().to_string();
        let branch_raw = dialog.branch.read(cx).text(cx);
        let branch = {
            let t = branch_raw.trim();
            if t.is_empty() {
                None
            } else {
                Some(t.to_string())
            }
        };
        self.new_task = None;
        match worktree_for {
            // A project's worktree is exactly that one project. Swapped in for
            // the create only, so the TASKS dialog's own project selection is
            // still there the next time it opens.
            Some(repo) => {
                let saved = (
                    std::mem::replace(&mut self.new_task_repos, vec![repo.clone()]),
                    std::mem::replace(&mut self.new_task_home, repo.clone()),
                    std::mem::replace(&mut self.new_target_repo, repo),
                );
                self.create_named(&name, &brief, branch.as_deref(), window, cx);
                (
                    self.new_task_repos,
                    self.new_task_home,
                    self.new_target_repo,
                ) = saved;
            }
            None => self.create_named(&name, &brief, branch.as_deref(), window, cx),
        }
    }

    /// Create a task from a name, optional brief, and optional branch override.
    /// An empty branch uses `feat/{name_slug}`.
    pub(crate) fn create_named(
        &mut self,
        name: &str,
        brief: &str,
        branch: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.data.repos.is_empty() {
            self.set_note("add a project first".to_string());
            cx.notify();
            return;
        }
        // Members: prefer explicit multi-select; fall back to new_target_repo.
        let mut member_names = self.new_task_repos.clone();
        if member_names.is_empty() {
            member_names.push(self.new_target_repo.clone());
        }
        member_names.retain(|n| self.data.repos.iter().any(|r| &r.name == n));
        if member_names.is_empty()
            && let Some(r) = self.data.repos.first()
        {
            member_names.push(r.name.clone());
        }
        let mut home = self.new_task_home.clone();
        if !member_names.iter().any(|n| n == &home) {
            home = member_names[0].clone();
        }

        let name_slug = task_name_slug(name);
        let branch = branch
            .map(str::trim)
            .filter(|b| !b.is_empty())
            .map(|b| b.to_string())
            .unwrap_or_else(|| format!("feat/{name_slug}"));
        let created_at = now_stamp();

        let members: Vec<CreateTaskMember> = member_names
            .iter()
            .filter_map(|n| {
                let repo = self.data.repos.iter().find(|r| &r.name == n)?;
                Some(CreateTaskMember {
                    repo: repo.name.clone(),
                    repo_root: repo.repo_root.clone(),
                    branch: branch.clone(),
                    base_ref: Some(repo.compare_ref.clone()),
                })
            })
            .collect();

        // Prefer an enabled local agent; never invent OpenCode when it's off.
        let agent_kind = default_enabled_agent(&self.prefs).unwrap_or(AgentKind::OpenCode);

        let req = CreateTaskRequest {
            name: name.to_string(),
            slug: name_slug,
            home_repo: home.clone(),
            members,
            created_at,
            agent_kind,
        };

        match create_task(&mut self.manager, &mut self.task_store, req) {
            Ok(task_id) => {
                let tasks_root = self.data_dir.join("tasks");
                // Multi-member coordination (+ optional brief) lives under
                // harness state (not project AGENTS.md). Setup runs visibly
                // in the shell after select so the user can see script output.
                let task = self.task_store.get(&task_id);
                if let Some(task) = task {
                    if let Err(e) = write_task_brief(&tasks_root, &task.slug, brief) {
                        log::warn!("could not write task brief: {e}");
                    }
                    if let Err(e) = write_task_agent_context(task, &tasks_root) {
                        log::warn!("could not write task agent context: {e}");
                    }
                }
                let multi = task.is_some_and(|t| t.members.len() >= 2);
                if multi {
                    self.ensure_task_broker(&task_id);
                }
                self.set_note(format!("created task {name}"));
                self.refresh(window, cx);
                self.select(
                    Selection::Task {
                        task_id: task_id.clone(),
                        member_id: None,
                    },
                    window,
                    cx,
                );
                self.run_visible_setup_hooks(window, cx);
                if multi {
                    self.open_start_specialists(task_id, cx);
                }
            }
            Err(e) => {
                self.set_note(format!("create failed: {e}"));
                cx.notify();
            }
        }
    }

    /// Run task + per-member setup hooks in the visible shell tab.
    ///
    /// Waits for the tmux session (the shell tab is created async by select)
    /// then sends each command so stdout/stderr appear in the terminal. Without
    /// tmux, falls back to a silent runner and surfaces the result in the note.
    pub(crate) fn run_visible_setup_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let Some(task_id) = r.task_id.clone() else {
            return;
        };
        let Some(task) = self.task_store.get(&task_id).cloned() else {
            return;
        };

        let task_hooks = TaskHooks::load(&self.data_dir, &task.slug);
        let member_paths = task.member_paths();
        let mut members: Vec<(HookEnv, Vec<PathBuf>)> = Vec::new();
        let mut untrusted: Vec<(String, PathBuf, Vec<PathBuf>)> = Vec::new();
        for m in &task.members {
            let Some(entry) = self.manager.registry.get(&m.worktree_id) else {
                continue;
            };
            let env = HookEnv {
                repo_root: entry.repo_root.clone(),
                worktree: m.path.clone(),
                branch: m.branch.clone(),
                task_slug: task.slug.clone(),
                task_id: Some(task_id.clone()),
                home_worktree: task.home_path().map(|p| p.to_path_buf()),
            };
            match HooksConfig::load_trusted(&entry.repo_root, &self.data_dir, &m.repo) {
                Ok(hooks) => {
                    if !hooks.setup.is_empty() {
                        members.push((env, hooks.setup));
                    }
                }
                Err(worktrees::HooksError::Untrusted { repo_root, scripts }) => {
                    untrusted.push((m.repo.clone(), repo_root, scripts));
                }
                Err(e) => {
                    self.set_note(format!("hooks load {}: {e}", m.repo));
                    cx.notify();
                }
            }
        }
        // A repo's own committed scripts never run unasked: hold everything
        // until the user has seen what they are (`render_hook_trust`).
        if !untrusted.is_empty() {
            self.hook_trust = Some(super::HookTrustPrompt { repos: untrusted });
            self.set_note("setup is waiting for you to approve the project's hooks".into());
            cx.notify();
            return;
        }
        let has_task_setup = task_hooks.setup.is_some();
        if !has_task_setup && members.is_empty() {
            return;
        }
        // Prefer a plain shell tab so setup output isn't mixed into an agent.
        let pane_key = r.key.clone();
        // Opening that shell makes it the active tab; remembered so an agent
        // that was in front can be put back.
        let active_before = self.panes.get(&pane_key).map(|p| p.active);
        if self.panes.get(&pane_key).is_none_or(|p| {
            !p.tabs
                .iter()
                .any(|t| t.agent.is_none() && matches!(t.content, TabContent::Terminal(_)))
        }) {
            self.open_tab(TabKind::Shell, window, cx);
        }
        let Some(pane) = self.panes.get_mut(&pane_key) else {
            return;
        };
        let Some(tab) = pane
            .tabs
            .iter()
            .find(|t| t.agent.is_none() && matches!(t.content, TabContent::Terminal(_)))
            .map(|t| t.id)
        else {
            return;
        };
        // Keep the agent in front when the pane has one (a coordinator opens
        // on its agent): setup output is a tab away, and the note says so.
        // Only an agent-less pane switches to the setup shell.
        let agent_before =
            active_before.filter(|a| pane.tabs.iter().any(|t| t.id == *a && t.agent.is_some()));
        let front = match agent_before {
            Some(agent) => agent,
            None if pane.tabs.iter().any(|t| t.agent.is_some()) => pane
                .tabs
                .iter()
                .filter(|t| t.agent.is_some())
                .map(|t| t.id)
                .min()
                .unwrap_or(tab),
            None => tab,
        };
        pane.active = front;
        self.save_panes();
        self.focus_active_pane(window, cx);

        if !self.has_tmux {
            // No visible session to drive — run once and report.
            let mut errors = Vec::new();
            let task_dir = self.data_dir.join("tasks").join(&task.slug);
            let _ = std::fs::create_dir_all(&task_dir);
            if let Err(e) =
                task_hooks.run_setup(self.manager.git().runner(), &task_dir, &member_paths)
            {
                errors.push(format!("task setup: {e}"));
            }
            for m in &task.members {
                let Some(entry) = self.manager.registry.get(&m.worktree_id) else {
                    continue;
                };
                let env = HookEnv {
                    repo_root: entry.repo_root.clone(),
                    worktree: m.path.clone(),
                    branch: m.branch.clone(),
                    task_slug: task.slug.clone(),
                    task_id: Some(task_id.clone()),
                    home_worktree: task.home_path().map(|p| p.to_path_buf()),
                };
                if let Ok(hooks) =
                    HooksConfig::load_trusted(&entry.repo_root, &self.data_dir, &m.repo)
                    && let Err(e) = hooks.run_setup(self.manager.git().runner(), &env)
                {
                    errors.push(format!("setup {}: {e}", m.repo));
                }
            }
            self.set_note(if errors.is_empty() {
                "setup finished".into()
            } else {
                errors.join("; ")
            });
            cx.notify();
            return;
        }

        let runner_path = self
            .data_dir
            .join("tasks")
            .join(&task.slug)
            .join(".setup-run.sh");
        if let Err(e) = write_visible_setup_runner(
            &runner_path,
            task_hooks.setup.as_deref(),
            &member_paths,
            &members,
        ) {
            self.set_note(format!("write setup runner: {e}"));
            cx.notify();
            return;
        }
        let invoke = visible_setup_shell_line(&runner_path);

        let session = crate::tmux::session_name(&pane_key, tab);
        self.set_note("running setup… (see shell)".into());
        cx.notify();

        cx.spawn_in(window, async move |view, cx| {
            // The shell tab creates its tmux session asynchronously; wait for it
            // before send-keys or the line vanishes into a missing session.
            for _ in 0..40 {
                let live = crate::tmux::live_sessions();
                if crate::tmux::is_live(&session, &live) {
                    break;
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
            }
            // Settle so the interactive shell has painted its first prompt.
            // One send-keys only — rapid multi-line dumps race fish/zsh and
            // leave later commands unexecuted in the typeahead buffer.
            cx.background_executor()
                .timer(std::time::Duration::from_millis(400))
                .await;

            crate::tmux::send_keys_async(&session, &invoke);

            let _ = view.update(cx, |ws, cx| {
                ws.set_note("setup finished (see shell)".into());
                cx.notify();
            });
        })
        .detach();
    }
}
