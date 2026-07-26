//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

impl HarnessWorkspace {

    pub(crate) fn select(&mut self, sel: Selection, window: &mut Window, cx: &mut Context<Self>) {
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
        self.data = WorkspaceData::build(self.manager.git(), &self.manager.registry, &self.repo_roots);

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
        self.select(Selection::Main(name), window, cx);
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
                    let _ = self.manager.registry.rename(&id, name);
                    let _ = self.manager.registry.save();
                }
                self.refresh(window, cx);
            }
            EditKind::Avatar(id) => {
                let initials = text.trim().to_uppercase();
                if !initials.is_empty()
                    && let Some(e) = self.manager.registry.get(&id) {
                        let color = e.avatar.color;
                        let _ = self
                            .manager
                            .registry
                            .set_avatar(&id, worktrees::Avatar { initials, color });
                        let _ = self.manager.registry.save();
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
        let target = self.new_target_repo.clone();
        let Some(repo) = self.data.repos.iter().find(|r| r.name == target).or_else(|| self.data.repos.first()) else {
            self.note = "add a project first".to_string();
            cx.notify();
            return;
        };
        let spec = NewWorktree::parse(input, &repo.compare_ref);
        let name = spec.name.as_str();
        let slug = slugify(name);
        let req = CreateRequest {
            repo: repo.name.clone(),
            repo_root: repo.repo_root.clone(),
            task_slug: slug.clone(),
            branch: spec.branch.clone().unwrap_or_else(|| format!("feat/{slug}")),
            base_ref: Some(spec.base_ref.clone()),
            created_at: now_stamp(),
            // OpenCode is the default (§4.5). It has the best signal of any
            // adapter we have: `permission.updated` over its local SSE stream
            // is a real, out-of-band "blocked on you" event, so its fidelity is
            // Exact and it may raise notifications — where pi's is BestEffort,
            // approximated from a status file an extension has to write. Less
            // machinery and a truer answer.
            //
            // pi stays a first-class peer rather than the default because what
            // it uniquely offers is the training loop (§4.4), not the signal.
            // Choosing per worktree is the natural next step in the create
            // dialog; until then this is the one that works with no setup.
            agent_kind: AgentKind::OpenCode,
        };
        match self.manager.create(req) {
            Ok(id) => {
                let _ = self.manager.registry.rename(&id, name);
                let _ = self.manager.registry.save();
                self.note = format!("created {id}");
                self.refresh(window, cx);
                self.select(Selection::Worktree(id), window, cx);
            }
            Err(e) => {
                self.note = format!("create failed: {e}");
                cx.notify();
            }
        }
    }

    pub(crate) fn set_avatar_color(&mut self, id: &str, color: u8, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(e) = self.manager.registry.get(id) {
            let initials = e.avatar.initials.clone();
            let _ = self
                .manager
                .registry
                .set_avatar(id, worktrees::Avatar { initials, color });
            let _ = self.manager.registry.save();
        }
        self.refresh(window, cx);
    }

    pub(crate) fn archive(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match self.manager.archive(&id) {
            Ok(()) => self.note = format!("archived {id}"),
            Err(e) => self.note = format!("archive failed: {e}"),
        }
        // Archiving stops the worktree's agents (freeing resources) and
        // purges its tab record, so unarchiving later starts fresh instead of
        // leaking a detached session or resurrecting a stale one.
        self.teardown_sessions(&id);
        self.panes.remove(&id);
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
        }
    }

    pub(crate) fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match self.manager.delete(&id) {
            Ok(()) => self.note = format!("deleted {id}"),
            Err(e) => self.note = format!("delete failed: {e}"),
        }
        // Teardown first (it reads the persisted record); then drop the live
        // pane client. Otherwise a later worktree with the same id could
        // reattach to this one's still-running agent session.
        self.teardown_sessions(&id);
        self.panes.remove(&id);
        self.pending_delete = None;
        self.refresh(window, cx);
        if let Some(name) = self.data.repos.first().map(|r| r.name.clone()) {
            self.select(Selection::Main(name), window, cx);
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
        };

        let base = div()
            .id(match target {
                Drag::Sidebar => "split-sidebar",
                Drag::DiffPanel => "split-diff",
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
        };
        let delta = f32::from(pos - drag.start) * sign;
        let next = drag.start_value + delta;
        match drag.target {
            Drag::Sidebar => self.prefs.sidebar_width = next,
            Drag::DiffPanel => self.prefs.diff_width = next,
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
