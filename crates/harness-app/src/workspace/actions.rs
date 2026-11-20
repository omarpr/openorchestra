//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

mod hooks_edit;
mod tasks;
mod window;

impl HarnessWorkspace {
    pub(crate) fn select(&mut self, sel: Selection, window: &mut Window, cx: &mut Context<Self>) {
        // Heal any hooks-poisoned Main editor before leaving this selection.
        self.exit_hooks_edit(window, cx);
        self.selection = sel;
        self.menu = None;
        self.tab_context_menu = None;
        self.edit = None;
        if let Selection::Task { task_id, .. } = &self.selection {
            let tid = task_id.clone();
            self.ensure_task_broker(&tid);
        }
        self.ensure_pane(window, cx);
        // Ensure the Files/Git host exists for this project so the explorer
        // shows the newly selected tree (not an empty "open editor" hint).
        let _ = self.ensure_service_workspace(window, cx);
        if self.prefs.diff_open
            && matches!(self.prefs.tool_tab, ToolTab::Files | ToolTab::Git)
            && self.files_sub_tab != FilesSubTab::Search
        {
            self.activate_hosted_tool_panel(self.prefs.tool_tab, window, cx);
        }
        // Retarget an in-progress search at the new project's root.
        if self.files_sub_tab == FilesSubTab::Search {
            self.schedule_project_search(window, cx);
        }
        self.recompute_diff(cx);
        self.focus_active_pane(window, cx);
        // The hosted zed workspaces set the window title themselves (folder —
        // file), and only when *their* active item changes. Switching to
        // another worktree changed neither, so the title kept naming the
        // previous project. Reset it to the newly selected folder; the new
        // worktree's workspace takes over again on its next file change.
        if let Some(r) = self.resolve()
            && let Some(name) = r.editor_path.file_name()
        {
            window.set_window_title(&name.to_string_lossy());
        }
        cx.notify();
    }

    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.data = WorkspaceData::build(
            self.manager.git(),
            &self.manager.registry,
            &self.task_store,
            &self.repo_roots,
            Some(&self.data),
        );
        // The rebuild carries the old badge numbers; fetch fresh ones off the
        // render thread (new cards start at zero until this lands).
        self.refresh_status_live(cx);
        // Follow the repo list: a repo added here has no `.git` watch yet, and a
        // removed one would keep firing recomputes for cards that are gone.
        self.git_watch.watch_repos(&self.repo_roots);

        self.sync_forge_branches();
        if let Some(source) = &mut self.status_source {
            for wt in self.data.repos.iter().flat_map(|repo| &repo.worktrees) {
                let path = &wt.path;
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
            // And each multi-project task's coordinator (task folder).
            let tasks_root = self.data_dir.join("tasks");
            for task in self
                .task_store
                .active()
                .filter(|t| super::coordinator::has_coordinator(t))
            {
                let dir = worktrees::task_coordinator_dir(&tasks_root, &task.slug);
                let _ = source.watch(&dir, task.agent_kind.clone());
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
                && let Some(path) = paths.into_iter().next()
            {
                let _ = view.update_in(cx, |ws, window, cx| ws.add_repo(path, window, cx));
            }
        })
        .detach();
    }

    pub(crate) fn add_repo(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(root) = self.manager.git().repo_root(&path) else {
            self.set_note("not a git repository".to_string());
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
            self.set_note(if resolved_up {
                format!("‘{picked}’ is inside ‘{name}’, which is already added")
            } else {
                format!("‘{name}’ is already added")
            });
            self.refresh(window, cx);
            self.select(Selection::Main(name), window, cx);
            return;
        }

        // Re-adding an archived project restores it rather than starting fresh.
        if relocate_root(&mut self.archived_repos, &mut self.repo_roots, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            self.set_note(format!("restored ‘{name}’ from archive"));
            self.refresh(window, cx);
            self.select(Selection::Main(name), window, cx);
            return;
        }

        self.repo_roots.push(root);
        save_repos(&self.repos_path, &self.repo_roots);
        self.set_note(if resolved_up {
            format!("added ‘{name}’ (the git repo containing ‘{picked}’)")
        } else {
            format!("added ‘{name}’")
        });
        self.refresh(window, cx);
        self.select(Selection::Main(name.clone()), window, cx);
        // Offer setup-script editing right after add — TOML stays hidden.
        self.open_project_hooks(&name, HookScriptPhase::Setup, HookLayer::Repo, window, cx);
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

    /// After a refresh that followed the other build's changes: if the
    /// selected project or task is gone, select as after a removal.
    fn reselect_if_gone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let gone = match &self.selection {
            Selection::Main(name) => !self.data.repos.iter().any(|r| &r.name == name),
            Selection::Task { task_id, .. } => !self.data.tasks.iter().any(|t| &t.id == task_id),
            Selection::Worktree(_) => false,
        };
        if gone {
            self.reselect_after_removal(window, cx);
        }
    }

    /// Archive a whole project: move its root to the restorable archived list.
    /// Non-destructive — worktrees and files stay on disk; the group just leaves
    /// the active sidebar until restored (from the Archived section, or by
    /// adding it again).
    pub(crate) fn archive_repo(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.repo_root_for_name(&name) else {
            return;
        };
        if relocate_root(&mut self.repo_roots, &mut self.archived_repos, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            self.set_note(format!("archived ‘{name}’"));
        }
        self.refresh(window, cx);
        self.reselect_after_removal(window, cx);
    }

    /// Ask before removing: opens the confirm dialog for `removal`.
    pub(crate) fn request_removal(&mut self, removal: super::Removal, cx: &mut Context<Self>) {
        self.confirm_removal = Some(removal);
        cx.notify();
    }

    /// Drop project `dragged` onto `onto`'s position (reorder panel).
    pub(crate) fn move_repo(
        &mut self,
        dragged: &str,
        onto: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(from), Some(to)) = (
            self.repo_root_for_name(dragged),
            self.repo_root_for_name(onto),
        ) else {
            return;
        };
        self.apply_repo_order(
            |roots| reordered(roots, from.clone(), to.clone()),
            window,
            cx,
        );
    }

    /// Drop task `dragged` onto `onto`'s position (reorder panel). The move
    /// is applied to tasks.json as it is on disk now, so tasks the other
    /// build (Dev / installed release) added meanwhile are kept, and shown.
    pub(crate) fn move_task(
        &mut self,
        dragged: &str,
        onto: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match move_task_on_disk(&self.task_store, dragged, onto) {
            Ok(Some(store)) => self.task_store = store,
            Ok(None) => return,
            Err(e) => {
                self.set_note(format!("couldn't save the task order: {e}"));
                cx.notify();
                return;
            }
        }
        let active: Vec<&str> = self.task_store.active().map(|t| t.id.as_str()).collect();
        let shown: Vec<&str> = self.data.tasks.iter().map(|t| t.id.as_str()).collect();
        if !same_members(&active, &shown) {
            // The other build added or removed tasks: rebuild the views.
            self.refresh(window, cx);
            self.reselect_if_gone(window, cx);
            return;
        }
        let order: Vec<String> = self
            .task_store
            .tasks()
            .iter()
            .map(|t| t.id.clone())
            .collect();
        self.data.tasks.sort_by_key(|t| {
            order
                .iter()
                .position(|id| *id == t.id)
                .unwrap_or(usize::MAX)
        });
        cx.notify();
    }

    /// "Move up" / "Move down" from the project menu.
    pub(crate) fn shift_repo(
        &mut self,
        name: &str,
        up: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.repo_root_for_name(name) else {
            return;
        };
        self.apply_repo_order(|roots| shifted(roots, &root, up), window, cx);
    }

    /// Apply `reorder` to the project list as it is on disk now (the other
    /// build may have added or removed projects; see `reorder_base`),
    /// persist it, and re-sort the loaded views to match — without a full
    /// refresh (which would re-run git for every repo) unless the set of
    /// projects changed.
    fn apply_repo_order(
        &mut self,
        reorder: impl FnOnce(&[PathBuf]) -> Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let base = reorder_base(load_repos(&self.repos_path), &self.repo_roots);
        let order = reorder(&base);
        if order == base {
            return;
        }
        save_repos(&self.repos_path, &order);
        let same = same_members(&order, &self.repo_roots);
        self.repo_roots = order;
        if !same {
            // Projects the other build added, archived or removed: follow its
            // archive list too, so a later save here can't drop an entry it
            // archived; then rebuild the views.
            self.archived_repos = load_repos(&self.archived_path);
            self.refresh(window, cx);
            self.reselect_if_gone(window, cx);
            return;
        }
        let roots = &self.repo_roots;
        self.data.repos.sort_by_key(|r| {
            roots
                .iter()
                .position(|p| *p == r.repo_root)
                .unwrap_or(usize::MAX)
        });
        cx.notify();
    }

    /// Whether project `name` can move up / down (it isn't already at that edge).
    pub(crate) fn repo_can_shift(&self, name: &str, up: bool) -> bool {
        let Some(i) = self.data.repos.iter().position(|r| r.name == name) else {
            return false;
        };
        if up {
            i > 0
        } else {
            i + 1 < self.data.repos.len()
        }
    }

    /// Remove a project from openOrchestra entirely, once the dialog confirms.
    /// Files on disk are untouched — this only forgets the project; re-add it
    /// to bring it back. Distinct from Archive, which keeps it restorable.
    pub(crate) fn remove_repo(
        &mut self,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(root) = self.repo_root_for_name(&name) {
            self.repo_roots.retain(|p| p != &root);
            save_repos(&self.repos_path, &self.repo_roots);
            self.set_note(format!("removed ‘{name}’ (files kept on disk)"));
        }
        self.refresh(window, cx);
        self.reselect_after_removal(window, cx);
    }

    /// Drop a project from the archived list. Like trash, it only forgets the
    /// project — files and worktrees stay on disk, and adding it again brings
    /// it back.
    pub(crate) fn forget_archived(&mut self, root: &Path, cx: &mut Context<Self>) {
        let before = self.archived_repos.len();
        self.archived_repos.retain(|p| p != root);
        if self.archived_repos.len() != before {
            save_repos(&self.archived_path, &self.archived_repos);
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.set_note(format!(
                "removed ‘{name}’ from the archive (files kept on disk)"
            ));
        }
        cx.notify();
    }

    /// Restore an archived project back into the active list.
    pub(crate) fn restore_repo(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if relocate_root(&mut self.archived_repos, &mut self.repo_roots, &root) {
            save_repos(&self.repos_path, &self.repo_roots);
            save_repos(&self.archived_path, &self.archived_repos);
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.set_note(format!("restored ‘{name}’"));
        }
        self.refresh(window, cx);
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
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
    pub(crate) fn confirm_edit(
        &mut self,
        _: &menu::Confirm,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_task.is_some() {
            self.submit_new_task(window, cx);
            return;
        }
        self.commit_edit(window, cx);
    }

    /// Escape in an inline edit field (routed from `menu::Cancel`): abandon it.
    pub(crate) fn cancel_edit(
        &mut self,
        _: &menu::Cancel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_task.is_some() {
            self.close_new_task(window, cx);
            return;
        }
        self.edit = None;
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    pub(crate) fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(active) = self.edit.take() else {
            return;
        };
        let text = active.editor.read(cx).text(cx);
        match active.kind {
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
                self.settings_focus.focus(window, cx);
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
                    self.set_note(if slug.is_empty() {
                        "agent name can't be empty".to_string()
                    } else {
                        format!("'{slug}' is not a valid new agent name")
                    });
                }
                self.settings_focus.focus(window, cx);
                cx.notify();
            }
            EditKind::RemotePort => {
                match super::ui::settings_remote::parse_port(&text) {
                    Ok(port) => self.remote_set_port(port, cx),
                    Err(e) => self.remote.ui.port_error = Some(e),
                }
                self.settings_focus.focus(window, cx);
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
                self.set_note(format!("worktree {member_id} not found"));
                cx.notify();
                return;
            };
            self.focus_task_member_quietly(&member_id, window, cx);
            member_path.join(rel)
        } else {
            let Some(r) = self.resolve() else { return };
            r.editor_path.join(display_path)
        };

        let Some(hosted) = self.ensure_service_workspace(window, cx) else {
            return;
        };
        hosted.update(cx, |ws, cx| {
            ws.open_abs_path(abs.clone(), ::workspace::OpenOptions::default(), window, cx)
                .detach();
        });
        self.set_note(format!("opened {}", abs.display()));
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
}
