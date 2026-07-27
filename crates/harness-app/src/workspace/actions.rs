//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

impl HarnessWorkspace {

    pub(crate) fn select(&mut self, sel: Selection, window: &mut Window, cx: &mut Context<Self>) {
        // Heal any hooks-poisoned Main editor before leaving this selection.
        self.exit_hooks_edit(window, cx);
        self.selection = sel;
        self.pending_delete = None;
        self.pending_repo_trash = None;
        self.menu = None;
        self.edit = None;
        self.ensure_pane(window, cx);
        self.recompute_diff(cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data = WorkspaceData::build(
            self.manager.git(),
            &self.manager.registry,
            &self.task_store,
            &self.repo_roots,
        );
        // Follow the repo list: a repo added here has no `.git` watch yet, and a
        // removed one would keep firing recomputes for cards that are gone.
        self.git_watch.watch_repos(&self.repo_roots);

        let items: Vec<(PathBuf, String, PathBuf)> = self
            .data
            .repos
            .iter()
            .flat_map(|repo| {
                repo.worktrees
                    .iter()
                    .map(move |w| (repo.repo_root.clone(), w.branch.clone(), w.path.clone()))
            })
            .collect();
        if let Some(p) = &self._poller {
            for (root, branch, _) in &items {
                p.add_branch(BranchKey {
                    repo_dir: root.clone(),
                    branch: branch.clone(),
                });
            }
            // Default branches for main-checkout CI (first-class main).
            for repo in &self.data.repos {
                p.add_branch(BranchKey {
                    repo_dir: repo.repo_root.clone(),
                    branch: repo.default_branch.clone(),
                });
            }
        }
        if let Some(source) = &mut self.status_source {
            for (_, _, path) in &items {
                let kind = self
                    .manager
                    .registry
                    .entries()
                    .iter()
                    .find(|e| &e.path == path)
                    .map(|e| e.agent_kind.clone())
                    .unwrap_or_default();
                let _ = source.watch(path, kind);
            }
            // Watch each repo root so an agent on `main` drives its pill.
            // `watch` is idempotent — a root already watched is a no-op error.
            for root in &self.repo_roots {
                let _ = source.watch(root, AgentKind::default());
            }
        }

        self.ensure_pane(window, cx);
        self.recompute_diff(cx);
        cx.notify();
    }

    // ---- projects --------------------------------------------------------

    pub(crate) fn add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next() {
                    let _ = view.update_in(cx, |ws, window, cx| ws.add_repo(path, window, cx));
                }
        })
        .detach();
    }

    pub(crate) fn add_repo(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(root) = self.manager.git().repo_root(&path) else {
            self.note = "not a git repository".to_string();
            cx.notify();
            return;
        };
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // The picker returns whatever folder was chosen, but a repo is tracked
        // by its git top-level. When they differ, the user picked a subfolder —
        // say so, or "it added the parent" reads as a bug (it isn't).
        let resolved_up = root != path;
        let picked = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        if self.repo_roots.contains(&root) {
            // Already active. Make the no-op legible instead of a silent nothing.
            self.note = if resolved_up {
                format!("‘{picked}’ is inside ‘{name}’, which is already added")
            } else {
                format!("‘{name}’ is already added")
            };
            self.refresh(window, cx);
            self.select(Selection::Main(name), window, cx);
            return;
        }

        // Re-adding an archived project restores it rather than starting fresh.
        if relocate_root(&mut self.archived_repos, &mut self.repo_roots, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            self.note = format!("restored ‘{name}’ from archive");
            self.refresh(window, cx);
            self.select(Selection::Main(name), window, cx);
            return;
        }

        self.repo_roots.push(root);
        save_repos(&self.repos_path, &self.repo_roots);
        self.note = if resolved_up {
            format!("added ‘{name}’ (the git repo containing ‘{picked}’)")
        } else {
            format!("added ‘{name}’")
        };
        self.refresh(window, cx);
        self.select(Selection::Main(name.clone()), window, cx);
        // Offer setup-script editing right after add — TOML stays hidden.
        self.open_project_hooks(
            &name,
            HookScriptPhase::Setup,
            HookLayer::Repo,
            window,
            cx,
        );
    }

    /// The tracked git root for a repo group, by its display name.
    fn repo_root_for_name(&self, name: &str) -> Option<PathBuf> {
        self.data
            .repos
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.repo_root.clone())
    }

    /// Reselect a sane entry after a project leaves the active list, so the
    /// center/diff panels never point at a group that is gone.
    fn reselect_after_removal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        } else {
            cx.notify();
        }
    }

    /// Archive a whole project: move its root to the restorable archived list.
    /// Non-destructive — worktrees and files stay on disk; the group just leaves
    /// the active sidebar until restored (from the Archived section, or by
    /// adding it again).
    pub(crate) fn archive_repo(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root_for_name(&name) else { return };
        if relocate_root(&mut self.repo_roots, &mut self.archived_repos, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            self.note = format!("archived ‘{name}’");
        }
        self.pending_repo_trash = None;
        self.refresh(window, cx);
        self.reselect_after_removal(window, cx);
    }

    /// Remove a project from openOrchestra entirely (two-click confirm). Files
    /// on disk are untouched — this only forgets the project; re-add it to bring
    /// it back. Distinct from Archive, which keeps it in a restorable list.
    pub(crate) fn trash_repo(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending_repo_trash.as_deref() != Some(name.as_str()) {
            self.pending_repo_trash = Some(name.clone());
            self.note = format!("remove ‘{name}’? click the trash again to confirm (files are kept)");
            cx.notify();
            return;
        }
        self.pending_repo_trash = None;
        if let Some(root) = self.repo_root_for_name(&name) {
            self.repo_roots.retain(|p| p != &root);
            save_repos(&self.repos_path, &self.repo_roots);
            self.note = format!("removed ‘{name}’ (files kept on disk)");
        }
        self.refresh(window, cx);
        self.reselect_after_removal(window, cx);
    }

    /// Restore an archived project back into the active list.
    pub(crate) fn restore_repo(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if relocate_root(&mut self.archived_repos, &mut self.repo_roots, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.note = format!("restored ‘{name}’");
        }
        self.refresh(window, cx);
        let name = root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.select(Selection::Main(name), window, cx);
    }

    // ---- text edit -------------------------------------------------------

    /// Open an inline edit backed by a real single-line `Editor`. `seed` is the
    /// current value (pre-filled and selected so the first keystroke replaces
    /// it); `placeholder` shows when the field is empty.
    pub(crate) fn begin_edit(
        &mut self,
        kind: EditKind,
        seed: &str,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let seed = seed.to_string();
        let placeholder = placeholder.to_string();
        let editor = cx.new(|cx| {
            let mut e = Editor::single_line(window, cx);
            e.set_placeholder_text(placeholder.as_str(), window, cx);
            if !seed.is_empty() {
                // Pre-fill with the current value; the caret lands at the end,
                // and ⌘A selects all to retype from scratch.
                e.set_text(seed.as_str(), window, cx);
            }
            e
        });
        window.focus(&editor.focus_handle(cx), cx);
        self.edit = Some(ActiveEdit { kind, editor });
        self.menu = None;
        cx.notify();
    }

    /// The active edit's target, for the render sites that decide whether a
    /// given row/card is the one being edited.
    pub(crate) fn edit_kind(&self) -> Option<&EditKind> {
        self.edit.as_ref().map(|e| &e.kind)
    }

    /// The active edit's backing editor, for the render sites that draw it.
    pub(crate) fn edit_editor(&self) -> Option<Entity<Editor>> {
        self.edit.as_ref().map(|e| e.editor.clone())
    }

    /// Enter in an inline edit field (routed from the global `menu::Confirm`).
    pub(crate) fn confirm_edit(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        self.commit_edit(window, cx);
    }

    /// Escape in an inline edit field (routed from `menu::Cancel`): abandon it.
    pub(crate) fn cancel_edit(&mut self, _: &menu::Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        self.edit = None;
        cx.notify();
    }

    pub(crate) fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.edit.take() else { return };
        let text = active.editor.read(cx).text(cx);
        match active.kind {
            EditKind::NewName => {
                let name = text.trim().to_string();
                if !name.is_empty() {
                    self.create_named(&name, window, cx);
                } else {
                    cx.notify();
                }
            }
            EditKind::Rename(id) => {
                let name = text.trim();
                if !name.is_empty() {
                    if self.task_store.get(&id).is_some() {
                        let _ = self.task_store.rename(&id, name);
                        let _ = self.task_store.save();
                        // Keep member worktree display names in sync.
                        if let Some(task) = self.task_store.get(&id) {
                            for m in &task.members {
                                let _ = self.manager.registry.rename(&m.worktree_id, name);
                            }
                            let _ = self.manager.registry.save();
                        }
                    } else {
                        let _ = self.manager.registry.rename(&id, name);
                        let _ = self.manager.registry.save();
                    }
                }
                self.refresh(window, cx);
            }
            EditKind::AgentCmd(kind) => {
                let cmd = text.trim().to_string();
                // Preserve custom-ness: or_default yields custom:false, which
                // would drop a custom agent from the catalog after an edit.
                let is_custom_key = AgentKind::known().iter().all(|k| k.as_str() != kind);
                let entry = self.prefs.agents.entry(kind).or_default();
                if is_custom_key {
                    entry.custom = true;
                }
                entry.command = if cmd.is_empty() { None } else { Some(cmd) };
                save_prefs_to(&self.prefs_path, &self.prefs);
                cx.notify();
            }
            EditKind::NewAgent => {
                let slug = text.trim().to_string();
                if valid_agent_slug(&slug, &self.prefs) {
                    // Prefill the launch command with the slug itself so a
                    // plain `cursor-agent` works the moment it is added; the
                    // user can refine it from the row afterwards.
                    self.prefs.agents.insert(
                        slug.clone(),
                        AgentPref {
                            disabled: false,
                            command: Some(slug.clone()),
                            custom: true,
                            signal: None,
                        },
                    );
                    save_prefs_to(&self.prefs_path, &self.prefs);
                } else {
                    self.note = if slug.is_empty() {
                        "agent name can't be empty".to_string()
                    } else {
                        format!("'{slug}' is not a valid new agent name")
                    };
                }
                cx.notify();
            }
        }
    }

    /// Also fix the note so it names the branch and base actually used.
    pub(crate) fn create_named(&mut self, input: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.data.repos.is_empty() {
            self.note = "add a project first".to_string();
            cx.notify();
            return;
        }
        // Members: prefer explicit multi-select; fall back to new_target_repo.
        let mut member_names = self.new_task_repos.clone();
        if member_names.is_empty() {
            member_names.push(self.new_target_repo.clone());
        }
        member_names.retain(|n| self.data.repos.iter().any(|r| &r.name == n));
        if member_names.is_empty() {
            if let Some(r) = self.data.repos.first() {
                member_names.push(r.name.clone());
            }
        }
        let mut home = self.new_task_home.clone();
        if !member_names.iter().any(|n| n == &home) {
            home = member_names[0].clone();
        }

        let primary = self
            .data
            .repos
            .iter()
            .find(|r| r.name == home)
            .or_else(|| self.data.repos.first())
            .unwrap();
        let spec = NewWorktree::parse(input, &primary.compare_ref);
        let name = spec.name.as_str();
        let slug = slugify(name);
        let branch = spec
            .branch
            .clone()
            .unwrap_or_else(|| format!("feat/{slug}"));
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

        let req = CreateTaskRequest {
            name: name.to_string(),
            slug: slug.clone(),
            home_repo: home.clone(),
            members,
            created_at,
            agent_kind: AgentKind::OpenCode,
        };

        match create_task(&mut self.manager, &mut self.task_store, req) {
            Ok(task_id) => {
                // AGENTS.md first; setup runs visibly in the shell after select
                // so the user can see script output (silent CommandRunner hid
                // failures behind the "created task" note).
                let task = self.task_store.get(&task_id).cloned();
                if let Some(task) = &task {
                    if let Some(home_m) = task.home_member() {
                        if let Some(entry) = self.manager.registry.get(&home_m.worktree_id) {
                            let _ = write_home_agents_md_at(
                                self.manager.git(),
                                task,
                                &entry.repo_root,
                            );
                        }
                    }
                }
                self.note = format!("created task {task_id}");
                self.refresh(window, cx);
                self.select(
                    Selection::Task {
                        task_id,
                        member_id: None,
                    },
                    window,
                    cx,
                );
                self.run_visible_setup_hooks(window, cx);
            }
            Err(e) => {
                self.note = format!("create failed: {e}");
                cx.notify();
            }
        }
    }

    /// Run task + per-member setup hooks in the visible shell tab.
    ///
    /// Waits for the tmux session (the shell tab is created async by select)
    /// then sends each command so stdout/stderr appear in the terminal. Without
    /// tmux, falls back to a silent runner and surfaces the result in the note.
    fn run_visible_setup_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        let Some(task_id) = r.task_id.clone() else { return };
        let Some(task) = self.task_store.get(&task_id).cloned() else {
            return;
        };

        let task_hooks = TaskHooks::load(&self.data_dir, &task.slug);
        let member_paths = task.member_paths();
        let mut members: Vec<(HookEnv, Vec<PathBuf>)> = Vec::new();
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
            match HooksConfig::load(&entry.repo_root, &self.data_dir, &m.repo) {
                Ok(hooks) => {
                    if !hooks.setup.is_empty() {
                        members.push((env, hooks.setup));
                    }
                }
                Err(e) => {
                    self.note = format!("hooks load {}: {e}", m.repo);
                    cx.notify();
                }
            }
        }
        let has_task_setup = task_hooks.setup.is_some();
        if !has_task_setup && members.is_empty() {
            return;
        }
        // Prefer a plain shell tab so setup output isn't mixed into an agent.
        let pane_key = r.key.clone();
        if self
            .panes
            .get(&pane_key)
            .is_none_or(|p| !p.tabs.iter().any(|t| t.agent.is_none() && matches!(t.content, TabContent::Terminal(_))))
        {
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
        pane.active = tab;
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
                if let Ok(hooks) = HooksConfig::load(&entry.repo_root, &self.data_dir, &m.repo) {
                    if let Err(e) = hooks.run_setup(self.manager.git().runner(), &env) {
                        errors.push(format!("setup {}: {e}", m.repo));
                    }
                }
            }
            self.note = if errors.is_empty() {
                "setup finished".into()
            } else {
                errors.join("; ")
            };
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
            self.note = format!("write setup runner: {e}");
            cx.notify();
            return;
        }
        let invoke = visible_setup_shell_line(&runner_path);

        let session = crate::tmux::session_name(&pane_key, tab);
        self.note = "running setup… (see shell)".into();
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

            crate::tmux::send_keys(&session, &invoke);

            let _ = view.update(cx, |ws, cx| {
                ws.note = "setup finished (see shell)".into();
                cx.notify();
            });
        })
        .detach();
    }

    /// Leave hooks-edit mode and restore a normal project editor on Main.
    ///
    /// Opening a Local script used to call `open_abs_path` on a path under
    /// `~/.openorchestra/hooks/…`, which poisons the hosted zed Project (empty
    /// file tree). Exiting always rebuilds the Main editor against the repo
    /// root so the project panel works again.
    pub(crate) fn exit_hooks_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((repo, _, _)) = self.hooks_edit.take() else {
            return;
        };
        if matches!(&self.selection, Selection::Main(n) if n == &repo)
            && let Some(repo_root) = self.repo_root_for_name(&repo)
        {
            let _ = self.replace_editor_workspace(&repo_root, window, cx);
        }
        cx.notify();
    }

    /// Replace (or create) the active pane's Editor tab with a workspace rooted
    /// at `root`. Returns the hosted workspace.
    fn replace_editor_workspace(
        &mut self,
        root: &std::path::Path,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        let hosted = match crate::workspace_host::workspace_for(root, window, cx) {
            Ok(ws) => {
                crate::workspace_host::probe(&ws, window, cx);
                cx.observe(&ws, |_, _, cx| cx.notify()).detach();
                ws
            }
            Err(e) => {
                self.note = format!("could not open project: {e}");
                cx.notify();
                return None;
            }
        };
        if let Some(pane) = self.panes.get_mut(&key) {
            if let Some(tab) = pane
                .tabs
                .iter_mut()
                .find(|t| matches!(t.content, TabContent::Editor(_)))
            {
                let id = tab.id;
                tab.content = TabContent::Editor(hosted.clone());
                pane.active = id;
                self.focus_active_pane(window, cx);
                self.save_panes();
                return Some(hosted);
            }
        }
        // No editor tab yet — fall through to the normal open path.
        self.open_tab(TabKind::Editor, window, cx);
        let pane = self.panes.get_mut(&key)?;
        if let Some(tab) = pane
            .tabs
            .iter_mut()
            .find(|t| matches!(t.content, TabContent::Editor(_)))
        {
            let id = tab.id;
            tab.content = TabContent::Editor(hosted.clone());
            pane.active = id;
            self.focus_active_pane(window, cx);
            self.save_panes();
            Some(hosted)
        } else {
            None
        }
    }

    /// Ensure a project hooks script exists and open it in the editor.
    ///
    /// Generates the layer's TOML if needed; the user only edits the shell
    /// script. Repo scripts open in a workspace rooted at the main checkout;
    /// Local scripts open in a workspace rooted at the local hooks dir so the
    /// project panel is not pointed at an out-of-tree abs path.
    pub(crate) fn open_project_hooks(
        &mut self,
        repo: &str,
        phase: HookScriptPhase,
        layer: HookLayer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(repo_root) = self.repo_root_for_name(repo) else {
            self.note = format!("project ‘{repo}’ not found");
            cx.notify();
            return;
        };
        let script = match ensure_hook_script(
            layer,
            phase,
            &repo_root,
            &self.data_dir,
            repo,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.note = format!("hooks: {e}");
                cx.notify();
                return;
            }
        };
        self.hooks_edit = Some((repo.to_string(), layer, phase));
        if !matches!(&self.selection, Selection::Main(n) if n == repo) {
            // select() clears hooks_edit — restore after.
            self.select(Selection::Main(repo.to_string()), window, cx);
            self.hooks_edit = Some((repo.to_string(), layer, phase));
        }
        let editor_root = match layer {
            HookLayer::Repo => repo_root.clone(),
            HookLayer::Local => local_hooks_base(&self.data_dir, repo),
        };
        let rel = match layer {
            HookLayer::Repo => format!(".openorchestra/scripts/{}", phase.file_name()),
            HookLayer::Local => format!("scripts/{}", phase.file_name()),
        };
        let Some(hosted) = self.replace_editor_workspace(&editor_root, window, cx) else {
            return;
        };
        crate::workspace_host::open_file_at(&hosted, &editor_root, &rel, None, window, cx);
        let layer_label = match layer {
            HookLayer::Repo => "repo",
            HookLayer::Local => "local",
        };
        self.note = format!(
            "editing {} {} ({})",
            phase.label(),
            layer_label,
            script.display()
        );
        cx.notify();
    }

    /// Drop the machine-local hooks override for `repo` and reopen the repo
    /// setup script (committed layer) so editing continues without local files.
    pub(crate) fn clear_project_local_hooks(
        &mut self,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match clear_local_hooks(&self.data_dir, repo) {
            Ok(()) => {
                self.note = format!("cleared local hooks for ‘{repo}’");
                self.open_project_hooks(
                    repo,
                    HookScriptPhase::Setup,
                    HookLayer::Repo,
                    window,
                    cx,
                );
            }
            Err(e) => {
                self.note = format!("clear local hooks: {e}");
                cx.notify();
            }
        }
    }

    /// Open a changed file from the DIFF panel in an editor tab.
    ///
    /// Reuses an existing editor tab when one is already open; otherwise opens
    /// a new editor for the focused worktree. For unified task diffs the
    /// display path is `repo/rel` — the matching member is focused quietly
    /// (without recomputing the diff) so the editor roots on that worktree.
    pub(crate) fn open_diff_file(
        &mut self,
        display_path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Leaving hooks mode first: otherwise the Repo/Local chrome stays up
        // and we reopen into the poisoned/wrong-rooted hooks editor.
        self.exit_hooks_edit(window, cx);

        let abs = if let Some(uf) = self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == display_path)
        {
            let member_id = uf.member_id.clone();
            let rel = uf.file.path.clone();
            let Some(member_path) = self
                .manager
                .registry
                .get(&member_id)
                .map(|e| e.path.clone())
            else {
                self.note = format!("worktree {member_id} not found");
                cx.notify();
                return;
            };
            self.focus_task_member_quietly(&member_id, window, cx);
            member_path.join(rel)
        } else {
            let Some(r) = self.resolve() else { return };
            r.editor_path.join(display_path)
        };

        let Some(hosted) = self.ensure_editor_tab(window, cx) else {
            return;
        };
        hosted.update(cx, |ws, cx| {
            ws.open_abs_path(
                abs.clone(),
                ::workspace::OpenOptions::default(),
                window,
                cx,
            )
            .detach();
        });
        self.note = format!("opened {}", abs.display());
        cx.notify();
    }

    /// Point the task selection at `member_id` without recomputing the DIFF
    /// (which would collapse the file the user just asked to edit).
    fn focus_task_member_quietly(
        &mut self,
        member_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match &self.selection {
            Selection::Task { .. } => {
                if let Selection::Task { member_id: mid, .. } = &mut self.selection {
                    *mid = Some(member_id.to_string());
                }
            }
            Selection::Worktree(id) => {
                let task_id = self
                    .manager
                    .registry
                    .get(id)
                    .and_then(|e| e.task_id.clone());
                if let Some(task_id) = task_id {
                    self.selection = Selection::Task {
                        task_id,
                        member_id: Some(member_id.to_string()),
                    };
                }
            }
            Selection::Main(_) => {
                // Hooks editing selects Main; opening a unified-diff file must
                // still land in the task member's editor, not the main checkout.
                if let Some(task_id) = self
                    .manager
                    .registry
                    .get(member_id)
                    .and_then(|e| e.task_id.clone())
                {
                    self.selection = Selection::Task {
                        task_id,
                        member_id: Some(member_id.to_string()),
                    };
                    self.ensure_pane(window, cx);
                }
            }
        }
    }

    /// Activate an existing editor tab, or open a new one. Returns the hosted
    /// zed Workspace so the caller can open a path into it.
    fn ensure_editor_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<::workspace::Workspace>> {
        let key = self.resolve()?.key;
        if let Some(pane) = self.panes.get(&key) {
            if let Some(tab) = pane
                .tabs
                .iter()
                .find(|t| matches!(t.content, TabContent::Editor(_)))
            {
                let id = tab.id;
                let hosted = match &tab.content {
                    TabContent::Editor(ws) => ws.clone(),
                    _ => unreachable!(),
                };
                if let Some(pane) = self.panes.get_mut(&key) {
                    pane.active = id;
                }
                self.focus_active_pane(window, cx);
                self.save_panes();
                return Some(hosted);
            }
        }
        self.open_tab(TabKind::Editor, window, cx);
        let pane = self.panes.get(&key)?;
        match &pane.active_tab()?.content {
            TabContent::Editor(ws) => Some(ws.clone()),
            _ => None,
        }
    }

    pub(crate) fn archive(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        // If this worktree belongs to a multi-member task, archive the whole task.
        let task_id = self
            .manager
            .registry
            .get(&id)
            .and_then(|e| e.task_id.clone())
            .or_else(|| {
                // id might already be a task id
                self.task_store.get(&id).map(|t| t.id.clone())
            });

        if let Some(tid) = task_id {
            if let Some(task) = self.task_store.get(&tid).cloned() {
                self.run_teardown_hooks(&task);
                let pane_key = format!("task:{tid}");
                for m in &task.members {
                    self.teardown_sessions(&m.worktree_id);
                    self.panes.remove(&m.worktree_id);
                }
                self.teardown_sessions(&pane_key);
                self.panes.remove(&pane_key);
                match archive_task(&mut self.manager, &mut self.task_store, &tid) {
                    Ok(()) => self.note = format!("archived task {tid}"),
                    Err(e) => self.note = format!("archive failed: {e}"),
                }
            }
        } else {
            self.run_member_teardown(&id);
            match self.manager.archive(&id) {
                Ok(()) => self.note = format!("archived {id}"),
                Err(e) => self.note = format!("archive failed: {e}"),
            }
            self.teardown_sessions(&id);
            self.panes.remove(&id);
        }
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        }
    }

    pub(crate) fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        let task_id = self
            .manager
            .registry
            .get(&id)
            .and_then(|e| e.task_id.clone())
            .or_else(|| self.task_store.get(&id).map(|t| t.id.clone()));

        if let Some(tid) = task_id {
            if let Some(task) = self.task_store.get(&tid).cloned() {
                self.run_teardown_hooks(&task);
                let pane_key = format!("task:{tid}");
                for m in &task.members {
                    self.teardown_sessions(&m.worktree_id);
                    self.panes.remove(&m.worktree_id);
                }
                self.teardown_sessions(&pane_key);
                self.panes.remove(&pane_key);
                match delete_task(&mut self.manager, &mut self.task_store, &tid) {
                    Ok(()) => self.note = format!("deleted task {tid}"),
                    Err(e) => self.note = format!("delete failed: {e}"),
                }
            }
        } else {
            self.run_member_teardown(&id);
            match self.manager.delete(&id) {
                Ok(()) => self.note = format!("deleted {id}"),
                Err(e) => self.note = format!("delete failed: {e}"),
            }
            self.teardown_sessions(&id);
            self.panes.remove(&id);
        }
        self.pending_delete = None;
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        }
    }

    fn run_teardown_hooks(&mut self, task: &worktrees::Task) {
        let task_hooks = TaskHooks::load(&self.data_dir, &task.slug);
        let member_paths = task.member_paths();
        let task_dir = self.data_dir.join("tasks").join(&task.slug);
        let _ = task_hooks.run_teardown(self.manager.git().runner(), &task_dir, &member_paths);
        for m in &task.members {
            self.run_member_teardown(&m.worktree_id);
        }
    }

    fn run_member_teardown(&mut self, worktree_id: &str) {
        let Some(entry) = self.manager.registry.get(worktree_id).cloned() else {
            return;
        };
        let env = HookEnv {
            repo_root: entry.repo_root.clone(),
            worktree: entry.path.clone(),
            branch: entry.branch.clone(),
            task_slug: entry.task_slug.clone(),
            task_id: entry.task_id.clone(),
            home_worktree: None,
        };
        if let Ok(hooks) = HooksConfig::load(&entry.repo_root, &self.data_dir, &entry.repo) {
            let _ = hooks.run_teardown(self.manager.git().runner(), &env);
        }
    }

    // ---- rendering -------------------------------------------------------

    /// Swap the palette and regenerate the zed theme from it, so editor,
    /// terminal, and the hosted workspace follow the chrome (§3.5, D7).
    /// A draggable splitter. `target` says which dimension it drives.
    ///
    /// 6px wide with a 1px visible line: narrower than this and it is fiddly to
    /// grab, wider and it reads as a gap in the chrome.
    pub(crate) fn splitter(&self, hx: &HarnessTheme, target: Drag, cx: &mut Context<Self>) -> impl IntoElement {
        // Both remaining splitters are vertical dividers between columns.
        let vertical = true;
        let active = self.drag.map(|d| d.target) == Some(target);
        let start_value = match target {
            Drag::Sidebar => self.prefs.sidebar_width,
            Drag::DiffPanel => self.prefs.diff_width,
            Drag::DiffList => self.prefs.diff_list_width,
        };

        let base = div()
            .id(match target {
                Drag::Sidebar => "split-sidebar",
                Drag::DiffPanel => "split-diff",
                Drag::DiffList => "split-diff-list",
            })
            .flex_none()
            .cursor(if vertical {
                gpui::CursorStyle::ResizeLeftRight
            } else {
                gpui::CursorStyle::ResizeUpDown
            })
            .when(active, |d| d.bg(hx.accent))
            .when(!active, |d| d.hover(|h| h.bg(hx.accent)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    ws.drag = Some(DragState {
                        target,
                        start: if vertical { ev.position.x } else { ev.position.y },
                        start_value,
                    });
                    cx.notify();
                }),
            );

        if vertical {
            base.w(px(6.0)).h_full()
        } else {
            base.h(px(6.0)).w_full()
        }
    }

    /// Apply an in-progress drag. Called from the root element so the pointer
    /// can leave the 6px handle without dropping the drag.
    pub(crate) fn drag_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag else { return };
        // The button can be released outside the window, in which case no
        // mouse-up ever reaches us; treat "moving with no button" as the end.
        if ev.pressed_button != Some(MouseButton::Left) {
            self.end_drag(cx);
            return;
        }
        let (pos, sign) = match drag.target {
            Drag::Sidebar => (ev.position.x, 1.0),
            // The diff panel is right-anchored, so dragging left widens it.
            Drag::DiffPanel => (ev.position.x, -1.0),
            // The file list is left-anchored inside the surface, so dragging
            // right widens it — the opposite of the right-anchored dock.
            Drag::DiffList => (ev.position.x, 1.0),
        };
        let delta = f32::from(pos - drag.start) * sign;
        let next = drag.start_value + delta;
        match drag.target {
            Drag::Sidebar => self.prefs.sidebar_width = next,
            Drag::DiffPanel => self.prefs.diff_width = next,
            Drag::DiffList => self.prefs.diff_list_width = next,
        }
        // Clamp every frame, not on release: a panel must never be draggable
        // to zero width and become unrecoverable.
        self.prefs = self.prefs.clone().sanitized();
        cx.notify();
    }

    pub(crate) fn end_drag(&mut self, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            save_prefs_to(&self.prefs_path, &self.prefs);
            cx.notify();
        }
    }

    pub(crate) fn save_prefs(&mut self, cx: &App) {
        self.prefs.dark = theme(cx).is_dark();
        save_prefs_to(&self.prefs_path, &self.prefs);
    }

    /// Live openOrchestra tmux sessions (our `harness_` prefix). Also catches
    /// orphaned sessions whose tab is gone, so "close terminals" really leaves
    /// nothing of ours running.
    fn live_harness_sessions(&self) -> Vec<String> {
        if !self.has_tmux {
            return Vec::new();
        }
        crate::tmux::live_sessions()
            .into_iter()
            .filter(|n| n.starts_with("harness_"))
            .collect()
    }

    /// Entry point for both quit paths (⌘Q / menu, and the window close button).
    /// Returns whether the app may close *now*: `true` when we've started the
    /// quit, `false` when we've instead opened the keep-or-close prompt and are
    /// waiting on the user.
    pub(crate) fn request_quit(&mut self, cx: &mut Context<Self>) -> bool {
        let sessions = self.live_harness_sessions();
        // Nothing of ours is running — no reason to ask.
        if sessions.is_empty() {
            cx.quit();
            return true;
        }
        match self.prefs.close_terminals_on_exit {
            // A remembered choice skips the prompt entirely.
            Some(kill) => {
                if kill {
                    self.kill_sessions(&sessions);
                }
                cx.quit();
                true
            }
            None => {
                self.close_prompt = Some(ClosePrompt {
                    remember: false,
                    session_count: sessions.len(),
                });
                cx.notify();
                false
            }
        }
    }

    /// Resolve the quit prompt. `keep` leaves the tmux sessions running
    /// (detached); otherwise they're killed. A ticked "remember" persists the
    /// choice so the prompt won't show again.
    pub(crate) fn resolve_quit(&mut self, keep: bool, cx: &mut Context<Self>) {
        let remember = self.close_prompt.as_ref().is_some_and(|p| p.remember);
        self.close_prompt = None;
        if remember {
            self.prefs.close_terminals_on_exit = Some(!keep);
            save_prefs_to(&self.prefs_path, &self.prefs);
        }
        if !keep {
            self.kill_sessions(&self.live_harness_sessions());
        }
        cx.quit();
    }

    /// Dismiss the quit prompt without quitting.
    pub(crate) fn cancel_quit(&mut self, cx: &mut Context<Self>) {
        self.close_prompt = None;
        cx.notify();
    }

    fn kill_sessions(&self, names: &[String]) {
        for name in names {
            crate::tmux::kill_session(name);
        }
    }

    /// Re-install the whole keymap and menus from the catalog + overrides.
    /// Clear-then-bind, not bind-on-top: gpui breaks precedence ties by
    /// recency, so stale chords would keep firing their old actions.
    pub(crate) fn apply_shortcuts(&self, cx: &mut Context<Self>) {
        cx.clear_key_bindings();
        crate::install_keymap(&self.prefs.shortcuts, cx);
        crate::install_menus(cx);
    }

    /// One captured chord while a shortcut row is armed.
    pub(crate) fn rebind_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.rebinding.clone() else { return };
        let ks = &event.keystroke;
        if ks.key == "escape" && !ks.modifiers.modified() {
            self.rebinding = None;
            cx.notify();
            cx.stop_propagation();
            return;
        }
        // A bare modifier press is the chord being formed, not the chord.
        if ["shift", "ctrl", "alt", "cmd", "platform", "function"].contains(&ks.key.as_str()) {
            cx.stop_propagation();
            return;
        }
        let mut chord = String::new();
        if ks.modifiers.control {
            chord.push_str("ctrl-");
        }
        if ks.modifiers.alt {
            chord.push_str("alt-");
        }
        if ks.modifiers.shift {
            chord.push_str("shift-");
        }
        if ks.modifiers.platform {
            chord.push_str("cmd-");
        }
        chord.push_str(&ks.key);
        self.prefs.shortcuts.insert(id, chord);
        save_prefs_to(&self.prefs_path, &self.prefs);
        self.rebinding = None;
        self.apply_shortcuts(cx);
        cx.notify();
        cx.stop_propagation();
    }

    /// Apply a zed theme by registry name (`None` = the harness pair).
    pub(crate) fn set_zed_theme(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.prefs.zed_theme = name;
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Set a font override. `mono` covers the buffer, the terminal, and the
    /// chrome's monospace; `!mono` is the interface font.
    pub(crate) fn set_font(&mut self, mono: bool, name: String, cx: &mut Context<Self>) {
        if mono {
            self.prefs.font_mono = Some(name);
        } else {
            self.prefs.font_ui = Some(name);
        }
        self.apply_appearance(cx);
        self.save_prefs(cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// Re-derive every appearance global from prefs: theme (harness pair or a
    /// zed registry theme), chrome tokens, fonts, and the zed settings JSON.
    /// One function on purpose — called at startup and after every settings
    /// change, so "restored" and "just picked" cannot diverge.
    pub(crate) fn apply_appearance(&mut self, cx: &mut Context<Self>) {
        // Fonts first: leaked to &'static str, matching HarnessTheme's fields
        // and theme.rs's own resolve_font. Bounded by the number of font
        // changes in a session.
        let cur = theme(cx).clone();
        let font_ui: &'static str = match &self.prefs.font_ui {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_ui,
        };
        let font_mono: &'static str = match &self.prefs.font_mono {
            Some(name) => Box::leak(name.clone().into_boxed_str()),
            None => cur.font_mono,
        };

        let zed_theme = self
            .prefs
            .zed_theme
            .as_ref()
            .and_then(|name| ::theme::ThemeRegistry::global(cx).get(name).ok());
        match zed_theme {
            Some(zed) => {
                ::theme::GlobalTheme::update_theme(cx, zed.clone());
                // Parsing is not coloring (see harness_zed_theme::install):
                // the registry's HighlightMaps rebuild from the new theme.
                ::workspace::AppState::global(cx).languages.set_theme(zed.clone());
                let hx = crate::harness_zed_theme::harness_from_zed(&zed, font_ui, font_mono);
                cx.set_global(hx);
            }
            None => {
                let mut hx = if self.prefs.dark {
                    HarnessTheme::dark(cx)
                } else {
                    HarnessTheme::light(cx)
                };
                hx.font_ui = font_ui;
                hx.font_mono = font_mono;
                crate::harness_zed_theme::install(&hx, cx);
                cx.set_global(hx);
            }
        }
        crate::apply_zed_settings(cx, font_ui, font_mono, self.prefs.font_size.unwrap_or(13.0));
    }
}
