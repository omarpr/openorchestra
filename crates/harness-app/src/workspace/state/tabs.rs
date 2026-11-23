//! The center tab strip: opening, closing, moving and focusing tabs, and
//! persisting/restoring them (with their tmux sessions) across restarts.

use super::*;

impl HarnessWorkspace {
    /// Open a new tab in the active worktree's set and focus it.
    pub(crate) fn open_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
        self.open_tab_with_review(kind, None, window, cx);
    }

    pub(crate) fn open_review_chat(
        &mut self,
        review: ReviewFollowup,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(kind) = review.cli.agent_kind() else {
            return;
        };
        self.open_tab_with_review(TabKind::Agent(kind), Some(review), window, cx);
    }

    fn open_tab_with_review(
        &mut self,
        kind: TabKind,
        review: Option<ReviewFollowup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        // `main` is a read-only checkout with no agent (D8).
        // D8 revised (July 22, 2026): agents may run on main too — it is the
        // user's repo and their call. Worktrees remain the *recommended*
        // isolation, and the chat pane stays worktree-only (it is pi's RPC
        // surface, wired to a worktree session).
        if matches!(kind, TabKind::Chat) && r.agent_kind.is_none() {
            self.set_note("agent chat needs a worktree".to_string());
            cx.notify();
            return;
        }

        // A blank page, after the active tab, with the address bar focused.
        if kind == TabKind::Browser {
            self.open_browser_tab(None, window, cx);
            return;
        }

        if kind == TabKind::ReviewOverview {
            self.tools_maximized = false;
            if let Some(pane) = self.panes.get_mut(&r.key)
                && let Some(tab) = pane
                    .tabs
                    .iter()
                    .find(|t| matches!(t.content, TabContent::ReviewOverview(_)))
            {
                pane.active = tab.id;
                self.focus_active_pane(window, cx);
                cx.notify();
                return;
            }
        }

        let tab_id = self.next_tab_id();

        let mut pending_agent_launch = None;
        let (content, title) = match kind {
            TabKind::Shell => {
                let session = self.has_tmux.then(|| {
                    let name = crate::tmux::session_name(&r.key, tab_id);
                    // Plus the zsh hook that logs commands for the phone's
                    // conversation view.
                    let mut env = self.pane_env(&r);
                    env.extend(self.remote.shell_env(&name));
                    crate::terminal_pane::TerminalSession {
                        name,
                        attach_only: false,
                        env,
                    }
                });
                (
                    TabContent::Terminal(
                        cx.new(|cx| TerminalPane::new(r.path.clone(), None, session, window, cx)),
                    ),
                    "❯_ shell".to_string(),
                )
            }
            TabKind::Agent(ref agent) => {
                let prep = self.prepare_agent_launch(agent, &r, tab_id);
                let session = self
                    .has_tmux
                    .then(|| crate::terminal_pane::TerminalSession {
                        name: crate::tmux::session_name(&r.key, tab_id),
                        attach_only: false,
                        env: self.pane_env(&r),
                    });
                let label = agent.as_str().to_string();
                let term_pane = cx.new(|cx| {
                    TerminalPane::new_pending(
                        r.path.clone(),
                        label.clone(),
                        session.as_ref().map(|s| s.name.clone()),
                        window,
                        cx,
                    )
                });
                pending_agent_launch = Some((prep, session));
                (TabContent::Terminal(term_pane), label)
            }
            // Nothing to construct: the surface reads state the view already
            // holds, so the tab is a marker saying where to draw it.
            TabKind::Diff => (TabContent::Diff, "⑂ diff".to_string()),
            TabKind::ReviewOverview => (
                TabContent::ReviewOverview(OverviewTab::default()),
                "Review Overview".to_string(),
            ),
            TabKind::Editor => {
                // One per worktree: a second ⌘E switches to the existing tab.
                if let Some(pane) = self.panes.get_mut(&r.key)
                    && let Some(existing) = pane
                        .tabs
                        .iter()
                        .find(|t| matches!(t.content, TabContent::Editor(_)))
                {
                    pane.active = existing.id;
                    self.focus_active_pane(window, cx);
                    cx.notify();
                    return;
                }
                let Some(ws) = self.ensure_ide_workspace(window, cx) else {
                    self.set_error_note("could not open the editor for this worktree".to_string());
                    cx.notify();
                    return;
                };
                (TabContent::Editor(ws), "{ } editor".to_string())
            }
            // Handled above (`open_browser_tab`).
            TabKind::Browser => return,
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
        // Every state probe passed while the screen showed nothing.
        self.attach_tab(
            &r.key,
            Tab {
                id: tab_id,
                title,
                content,
                agent: agent_of_kind,
            },
        );
        if let TabKind::Agent(a) = &kind {
            // The transcript on record is an earlier session's, not this tab's.
            self.remote.forget_session(&r.path, a.as_str());
        }
        self.focus_active_pane(window, cx);
        self.save_panes(cx);
        if kind == TabKind::ReviewOverview {
            self.run_review_overview(cx);
        }
        if let Some((prep, session)) = pending_agent_launch {
            self.pending_agent_launches.insert(tab_id);
            let key = r.key.clone();
            let session_name = session.as_ref().map(|session| session.name.clone());
            cx.spawn_in(window, async move |view, cx| {
                let resolved = cx
                    .background_executor()
                    .spawn(async move { prep.resolve(review) })
                    .await;
                let launch = view
                    .update_in(cx, |ws, window, cx| {
                        if resolved.review_cli_missing {
                            ws.abandon_missing_review_cli(
                                &key,
                                tab_id,
                                &resolved.kind_label,
                                window,
                                cx,
                            );
                            return None;
                        }
                        let command = resolved.command;
                        let pane = ws.panes.get_mut(&key)?;
                        let tab = pane.tabs.iter_mut().find(|t| t.id == tab_id)?;
                        let TabContent::Terminal(term_pane) = &tab.content else {
                            return None;
                        };
                        let term_pane = term_pane.clone();
                        let label = command
                            .as_ref()
                            .map(|c| c.label.clone())
                            .unwrap_or_else(|| "shell".to_string());
                        if tab.title != label {
                            tab.title = label;
                            ws.save_panes(cx);
                        }
                        let launch = term_pane
                            .update(cx, |pane, cx| pane.launch(command, session, window, cx));
                        cx.notify();
                        Some(launch)
                    })
                    .ok()
                    .flatten();
                let started = match launch {
                    Some(launch) => launch.await,
                    None => false,
                };
                if started && let Some(name) = session_name {
                    // A spawned client can still be creating its detached session.
                    cx.background_executor()
                        .spawn(async move { crate::tmux::wait_for_session(&name) })
                        .await;
                }
                let _ = view.update(cx, |ws, cx| {
                    ws.pending_agent_launches.remove(&tab_id);
                    if started {
                        ws.flush_a2a_inbox_for_pane(&key);
                    }
                    let active = ws.resolve().map(|r| r.key).unwrap_or_default();
                    ws.evict_stale_panes(&active, cx);
                    cx.notify();
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
            self.set_note(format!("{file} is deleted in this branch"));
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
        // A task's diff keys files `repo/path`: open it in its member's
        // worktree. Anything else is relative to the editor root, never
        // `r.path`: for a coordinator that is the task folder, which holds
        // the A2A bearer-key scripts and is no project to root an editor in.
        let (root, rel) = match self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == file)
        {
            Some(f) => (f.worktree.clone(), f.file.path.clone()),
            None => (r.editor_path.clone(), file),
        };
        self.open_workspace_file(&root, &rel, line, window, cx);
    }

    pub(crate) fn open_workspace_file(
        &mut self,
        root: &Path,
        relative: &str,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(hosted) = self.ensure_service_workspace(window, cx) else {
            self.set_error_note("could not open an editor".to_string());
            cx.notify();
            return;
        };
        crate::workspace_host::open_file_at(&hosted, root, relative, line, false, window, cx);
        cx.notify();
    }

    /// Move a tab within its own pane set, then persist the new order.
    ///
    /// Sorts the existing tabs rather than rebuilding them, so the live
    /// entities — a pty and the process on the other end of it — travel with
    /// their tab instead of being dropped and respawned.
    pub(crate) fn move_tab(&mut self, key: &str, dragged: u64, onto: u64, cx: &App) {
        let Some(pane) = self.panes.get_mut(key) else {
            return;
        };
        let ids: Vec<u64> = pane.tabs.iter().map(|t| t.id).collect();
        let order = reordered(&ids, dragged, onto);
        if order == ids {
            return;
        }
        pane.tabs
            .sort_by_key(|t| order.iter().position(|i| *i == t.id).unwrap_or(usize::MAX));
        self.save_panes(cx);
    }

    /// Close a tab, dropping whatever it held — a terminal's pty and its
    /// process, a chat's `pi --mode rpc` sidecar, or a service-Workspace item.
    /// Close a tab from the UI. Closing a task pane's agent tab stops one of
    /// the task's agents, so it asks first; everything else closes directly.
    pub(crate) fn request_close_tab(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = self.resolve() else { return };
        let agent_label = self
            .panes
            .get(&r.key)
            .and_then(|p| p.tabs.iter().find(|t| t.id == id))
            .filter(|t| t.agent.is_some())
            .map(|t| t.title.clone());
        let task_member = a2a_broker::parse_task_member_pane_key(&r.key)
            .map(|(tid, member)| (tid.to_string(), member.to_string()));
        let (Some(label), Some((tid, member))) = (agent_label, task_member) else {
            self.close_tab(id, window, cx);
            return;
        };
        let task_name = self
            .task_store
            .get(&tid)
            .map(|t| t.name.clone())
            .unwrap_or(tid);
        let who = if member == a2a_broker::COORDINATOR_TENANT {
            "coordinator".to_string()
        } else {
            member.split('/').next().unwrap_or(&member).to_string()
        };
        let message = format!("Stop the {who} agent for {task_name}?");
        let detail = format!(
            "This ends the {label} session. Its row shows STOPPED; click that to restart it where it left off."
        );
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &message,
            Some(&detail),
            &["Stop agent", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |ws, cx| {
            if answer.await.ok() == Some(0) {
                let _ = ws.update_in(cx, |ws, window, cx| ws.close_tab(id, window, cx));
            }
        })
        .detach();
    }

    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        self.tab_context_menu = None;
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let file_item = self.panes.get(&key).and_then(|p| {
            p.tabs
                .iter()
                .find(|t| t.id == id)
                .and_then(|t| match &t.content {
                    TabContent::File { item_id, .. } => Some(*item_id),
                    _ => None,
                })
        });
        if let Some(item_id) = file_item
            && let Some(hosted) = self.service_workspaces.get(&key).cloned()
        {
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
        // Is the tab being closed an agent tab? If so, killing its session ends
        // that agent, and a dead agent never writes the resolving "idle" — so its
        // pill would stay stuck on WAITING/RUNNING forever. Clear it here.
        let closed_agent = self
            .panes
            .get(&key)
            .and_then(|p| p.tabs.iter().find(|t| t.id == id))
            .and_then(|t| t.agent.clone());
        let was_agent = closed_agent.is_some();
        // Only a terminal tab has a tmux session; a browser tab (or any
        // other) would wait on tmux for nothing, up to its timeout.
        let terminal = self.panes.get(&key).is_some_and(|p| {
            p.tabs
                .iter()
                .any(|t| t.id == id && matches!(t.content, TabContent::Terminal(_)))
        });
        if self.has_tmux && terminal {
            // Explicit close tears the session down (only app-exit detaches).
            crate::tmux::kill_session(&crate::tmux::session_name(&key, id));
        }
        // A closed tab is never reachable from a phone again.
        self.remote_unmark(&key, Some(id));
        if let Some(a) = &closed_agent {
            // Its transcript must not resurface in the next tab of this kind.
            self.remote.forget_session(&r.path, a.as_str());
        }
        if was_agent {
            self.stop_agent(&crate::conductor::AgentTarget::new(&r.path, Some(id)));
            let _ = std::fs::remove_file(agent_runtime::status_path(&r.path, Some(id)));
            if !self.panes[&key]
                .tabs
                .iter()
                .any(|tab| tab.id != id && tab.agent.is_some())
            {
                self.stop_agent(&crate::conductor::AgentTarget::new(&r.path, None));
            }
        }
        let Some(pane) = self.panes.get_mut(&key) else {
            return;
        };
        let Some(ix) = pane.tabs.iter().position(|t| t.id == id) else {
            return;
        };
        // A closing browser tab takes its page down now, not when the
        // entity is finally released.
        if let TabContent::Browser(page) = &pane.tabs[ix].content {
            page.update(cx, |page, cx| page.set_shown(false, window, cx));
        }
        // The pane keeps its agent on record even when its last agent tab is
        // closed on purpose: a task needs every agent, so a closed one shows
        // STOPPED with a Restart like one lost to a computer restart.
        pane.remove_tab_at(ix);
        // A worktree with no tabs is a dead end; give it a shell back.
        if pane.tabs.is_empty() {
            self.open_tab(TabKind::Shell, window, cx); // saves panes itself
            return;
        }
        // The tab after it may be a browser tab: show its page.
        self.sync_browser_views(window, cx);
        self.focus_active_pane(window, cx);
        self.save_panes(cx);
        cx.notify();
    }

    /// Focus the active worktree's browser tab already showing `url`, or
    /// open a new browser tab just after the active tab (always a new, blank
    /// one for `None`). Returns the tab's id; `None` when no worktree is
    /// selected or its pane isn't live.
    pub(crate) fn open_browser_tab(
        &mut self,
        url: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let key = self.resolve()?.key;
        self.open_browser_tab_in(&key, url, BrowserOpen::ReuseSameUrl, window, cx)
    }

    /// [`Self::open_browser_tab`] in pane `key`, reusing a tab already on
    /// `url` only as `how` says: a page's new window opens beside it, in
    /// the pane that owns that page, always as a tab of its own.
    ///
    /// Only into a live pane: a pane created here would hold just this tab,
    /// and the next save would replace that worktree's saved tabs with it.
    fn open_browser_tab_in(
        &mut self,
        key: &str,
        url: Option<String>,
        how: BrowserOpen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        // Every way into a tab (a routed link, Ports' Open, a page's new
        // window) comes through here; WebKit refuses `0.0.0.0` and `[::]`,
        // so those load through loopback, and reuse matches what loads.
        let url = url
            .filter(|u| !u.is_empty())
            .map(|u| crate::link_routing::loopback_for_unspecified(&u));
        let records: Vec<_> = self
            .panes
            .get(key)?
            .tabs
            .iter()
            .filter_map(|t| t.record(cx))
            .collect();
        let existing = reusable_browser_tab(&records, url.as_deref(), how);
        let id = match existing {
            Some(id) => {
                if let Some(pane) = self.panes.get_mut(key) {
                    pane.active = id;
                }
                id
            }
            None => {
                let id = self.next_tab_id();
                // Made to load its page now: that load is a visit.
                let page = self.new_browser_pane(
                    key,
                    url,
                    None,
                    crate::browser_pane::PageLoad::Opened,
                    window,
                    cx,
                );
                let title = page.read(cx).label();
                self.pane_clock = self.pane_clock.wrapping_add(1);
                // Checked live above; nothing here creates a pane.
                let pane = self.panes.get_mut(key)?;
                pane.last_used = self.pane_clock;
                pane.insert_after_active(Tab {
                    id,
                    title,
                    content: TabContent::Browser(page),
                    agent: None,
                });
                self.save_panes(cx);
                id
            }
        };
        // The page shows only with the center drawn: an open into the
        // selected worktree (Ports' Open from a maximized tools panel, say)
        // restores it. A request that lands for another worktree, after the
        // user moved on, leaves the layout alone.
        if self.resolve().is_some_and(|r| r.key == key) {
            self.tools_maximized = false;
        }
        self.sync_browser_views(window, cx);
        self.focus_active_pane(window, cx);
        cx.notify();
        Some(id)
    }

    /// A browser pane for a tab in pane `key`, wired to the workspace: a
    /// changed URL or title relabels the tab at once, a changed URL also
    /// saves a moment later ([`Self::schedule_browser_save`]; a title rides
    /// along with the next save), and a page's new window opens as another
    /// browser tab in the same pane. The subscription ends when
    /// the pane is released. `load` says whether its first page view is a
    /// visit to record (`PageLoad`).
    fn new_browser_pane(
        &mut self,
        key: &str,
        url: Option<String>,
        title: Option<String>,
        load: crate::browser_pane::PageLoad,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<crate::browser_pane::BrowserPane> {
        use crate::browser_pane::{BrowserEvent, BrowserPane};
        let history = self.browser_history.clone();
        let project = self.browser_project(key);
        let page = cx.new(|cx| {
            let mut page = BrowserPane::new(url, title, load, history, window, cx);
            page.set_project(project, cx);
            page
        });
        let key = key.to_string();
        cx.subscribe_in(&page, window, move |ws, page, event: &BrowserEvent, window, cx| {
            match event {
                BrowserEvent::UrlChanged | BrowserEvent::TitleChanged => {
                    let label = page.read(cx).label();
                    if let Some(tab) = ws.panes.get_mut(&key).and_then(|p| {
                        p.tabs
                            .iter_mut()
                            .find(|t| matches!(&t.content, TabContent::Browser(b) if b == page))
                    }) {
                        tab.title = label;
                    }
                    if event.saves() {
                        ws.schedule_browser_save(cx);
                    }
                    cx.notify();
                }
                BrowserEvent::InspectorPlaced(placement) => {
                    // The pane already updated the shared mirror; only the
                    // file is left.
                    ws.prefs.inspector_placement = *placement;
                    ws.save_prefs(cx);
                }
                BrowserEvent::NewWindow(url) => {
                    // WebKit has already declined to open its own window;
                    // the page asked for a new one, so it gets a new tab
                    // even when one (its own, say) is on that URL.
                    ws.open_browser_tab_in(
                        &key,
                        Some(url.clone()),
                        BrowserOpen::AlwaysNew,
                        window,
                        cx,
                    );
                }
            }
        })
        .detach();
        page
    }

    /// Save the tabs [`BROWSER_SAVE_DELAY`] after the last browser URL
    /// change. A navigation brings several (redirects, a single-page app's
    /// `pushState`), and each full save rewrites and syncs three files on
    /// the UI thread, which WebKit's input shares. Title changes don't
    /// schedule one (an animated title would never stop), so a title is
    /// saved with the next save of any kind. Replacing
    /// the task cancels the previous one, so a burst ends in one save; any
    /// other save (open, close, move, quit, an eviction) clears it, having
    /// written the same state.
    fn schedule_browser_save(&mut self, cx: &mut Context<Self>) {
        self.browser_save = Some(cx.spawn(async move |ws, cx| {
            cx.background_executor().timer(BROWSER_SAVE_DELAY).await;
            let _ = ws.update(cx, |ws, cx| ws.save_panes(cx));
        }));
    }

    /// The project a browser tab in pane `key` belongs to, for its history:
    /// the repository the sidebar groups the pane's worktree under, keyed
    /// by that repository's main checkout. Every pane of a task (each
    /// member, the coordinator) belongs to the task's home repository.
    /// `None` while the pane's repository isn't known (not loaded yet, or
    /// removed).
    pub(crate) fn browser_project(&self, key: &str) -> Option<crate::browser_pane::BrowserProject> {
        let by_name = |name: &str| {
            self.repo_root_for_name(name).map(|root| (root, name.to_string()))
        };
        let (root, name) = match pane_owner(key) {
            PaneOwner::Main(name) => by_name(name)?,
            PaneOwner::Task(task_id) => {
                let task = self.task_store.get(task_id)?;
                // The home member's own registry entry names its main
                // checkout exactly; the repo name is the fallback.
                task.home_member()
                    .and_then(|m| self.manager.registry.get(&m.worktree_id))
                    .map(|e| (e.repo_root.clone(), e.repo.clone()))
                    .or_else(|| by_name(&task.home_repo))?
            }
            PaneOwner::Worktree(id) => {
                let entry = self.manager.registry.get(id)?;
                (entry.repo_root.clone(), entry.repo.clone())
            }
        };
        Some(crate::browser_pane::BrowserProject {
            key: root.to_string_lossy().into_owned(),
            name,
        })
    }

    /// The browser pane whose page should be on screen: the selected
    /// worktree's active tab, when it is a browser tab, the center is drawn
    /// (not covered by the maximized tools panel) and the window isn't
    /// minimized or hidden.
    fn shown_browser(&self) -> Option<EntityId> {
        if self.browser_window_hidden || (self.prefs.diff_open && self.tools_maximized) {
            return None;
        }
        let r = self.resolve()?;
        match &self.panes.get(&r.key)?.active_tab()?.content {
            TabContent::Browser(page) => Some(page.entity_id()),
            _ => None,
        }
    }

    /// Something gpui draws over the window is open: a menu, popover,
    /// dialog or screen. The native page sits above everything gpui draws,
    /// so while this is true it hides (see `BrowserPane::set_overlay_open`).
    /// Mirrors what `render` draws as an overlay; a new overlay belongs here.
    ///
    /// A failure toast counts while it shows (ruling R11): the user has to
    /// see it. Not counted, by design: info toasts, the conductor, tooltips
    /// and drag previews. They float over the window without taking it
    /// over, and hiding the page for each would blank it constantly. The
    /// conductor (his figure and his bubble or badge) is cut out of the page
    /// instead, so he shows through it and can be clicked and dragged there
    /// (`browser_pane::set_punch_through`); the rest, where they overlap the
    /// page, are under it. Native `window.prompt`
    /// sheets (NSAlert) are their own window, above the page, and need
    /// nothing.
    ///
    /// A browser tab's own address-bar suggestion list isn't listed here:
    /// the pane hides its page for it itself, in the same frame
    /// (`BrowserPane::page_visible`).
    pub(crate) fn overlay_open(&self, cx: &App) -> bool {
        self.tab_menu.is_some()
            || self.tab_context_menu.is_some()
            || self.comment_draft.is_some()
            || self.settings_open
            || self.preflight_open
            || self.menu.is_some()
            || self.project_menu.is_some()
            || self.reorder_panel.is_some()
            || self.diff_base_menu.is_some()
            || self.ports_sort_menu.is_some()
            || self.edit_prompt.is_some()
            || self.new_task.is_some()
            || self.start_specialists.is_some()
            || self.close_prompt.is_some()
            || self.confirm_removal.is_some()
            // Drawn once its loss check is in (`render_end_confirm`).
            || self.end_confirm.as_ref().is_some_and(|c| c.losses.is_some())
            || self.hook_trust.is_some()
            || self.device_shutdown_confirm.is_some()
            || self.failure_toast_showing()
            || self.zed_modal_open(cx)
            || super::super::ui::app_menu_open()
    }

    /// The one place that decides which browser tab has a native view: the
    /// one [`Self::shown_browser`] names, and no other, in any pane. Every
    /// other browser tab drops its view (it reloads when shown again). The
    /// shown one also hides while [`Self::overlay_open`].
    /// Idempotent; called on tab and worktree switches, on close, and at the
    /// top of every render as the catch-all.
    pub(crate) fn sync_browser_views(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown_browser();
        let overlay = self.overlay_open(cx);
        // The shown tab's project, in case its repository wasn't known when
        // the tab was made (restored before the data loaded).
        let shown_project = shown.and_then(|id| {
            self.panes
                .iter()
                .find(|(_, p)| {
                    p.tabs.iter().any(
                        |t| matches!(&t.content, TabContent::Browser(b) if b.entity_id() == id),
                    )
                })
                .and_then(|(key, _)| self.browser_project(key))
        });
        let pages: Vec<Entity<crate::browser_pane::BrowserPane>> = self
            .panes
            .values()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| match &t.content {
                TabContent::Browser(page) => Some(page.clone()),
                _ => None,
            })
            .collect();
        // Hide before show, so two pages are never up at once.
        let (show, hide): (Vec<_>, Vec<_>) = pages
            .into_iter()
            .partition(|page| Some(page.entity_id()) == shown);
        // A blank tab lists the task's servers: Ports' task rows, from its
        // sample, which is taken meanwhile (`ports_wanted`), starting now.
        let blank = show.iter().any(|page| page.read(cx).is_blank());
        if blank && !self.blank_browser_shown {
            self.blank_browser_shown = true;
            self.refresh_ports(cx);
        }
        self.blank_browser_shown = blank;
        let running: Vec<crate::browser_pane::RunningServer> = if blank && self.ports_current() {
            self.ports_view()
                .map(|view| {
                    view.task
                        .iter()
                        .map(|row| crate::browser_pane::RunningServer {
                            port: row.port,
                            detail: crate::workspace::ui::ports_panel::port_row_detail(row),
                            url: crate::ports::url_for(row),
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        for page in hide {
            page.update(cx, |page, cx| page.set_shown(false, window, cx));
        }
        for page in show {
            // Overlay first, so a view created under an open menu starts
            // hidden rather than flashing up for a frame.
            page.update(cx, |page, cx| {
                page.set_overlay_open(overlay);
                if let Some(project) = shown_project.clone() {
                    page.set_project(Some(project), cx);
                }
                page.set_running(running.clone());
                page.set_shown(true, window, cx);
            });
        }
    }

    /// gpui reports a covered window, another Space, a minimized window and
    /// a hidden app all as `Hidden`. Only the last two drop the page; AppKit
    /// settles those states around the occlusion change, so look again a
    /// moment later. Back to visible, the next render brings the page back.
    pub(crate) fn window_visibility_changed(
        &mut self,
        visibility: gpui::WindowVisibility,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if visibility.is_visible() {
            if self.browser_window_hidden {
                self.browser_window_hidden = false;
                cx.notify();
            }
            self.sync_device_visibility(window, cx);
            return;
        }
        cx.spawn_in(window, async move |ws, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = ws.update_in(cx, |ws, window, cx| {
                if !window.is_visible() && crate::browser_pane::window_is_minimized_or_hidden(window)
                {
                    ws.browser_window_hidden = true;
                    ws.sync_browser_views(window, cx);
                }
                // The Device tab stops streaming while minimised, hidden or
                // covered.
                ws.sync_device_visibility(window, cx);
            });
        })
        .detach();
    }

    /// Drop the least-recently-used pane sets beyond [`MAX_LIVE_PANES`],
    /// keeping the active pane and any panes still starting agents.
    ///
    /// `save_panes` records only live panes, so a browser change still
    /// waiting for its debounced save ([`Self::schedule_browser_save`]) is
    /// saved first: otherwise the evicted pane's new URL would be lost (its
    /// title rides along with that save), and reselecting the worktree
    /// would restore the old page.
    pub(crate) fn evict_stale_panes(&mut self, keep: &str, cx: &App) {
        while self.panes.len() > MAX_LIVE_PANES {
            let Some(victim) = pane_to_evict(&self.panes, keep, &self.pending_agent_launches)
            else {
                break;
            };
            let victim = victim.to_owned();
            if self.browser_save.is_some() {
                self.save_panes(cx);
            }
            log::info!("evicting pane {victim} (over the {MAX_LIVE_PANES}-pane cap)");
            // Dropping the terminal closes its pty and detaches the tmux client.
            self.panes.remove(&victim);
        }
    }

    pub(crate) fn focus_active_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open {
            // "Add a device" sits over Settings, outside its card: while it
            // is open the keyboard belongs to it.
            if self.remote.ui.setup_open
                && self.remote.serve == crate::workspace::remote::RemoteServe::Serving
            {
                self.remote_setup_focus.restore(window, cx);
            } else {
                self.settings_focus.restore(window, cx);
            }
            return;
        }
        let Some(r) = self.resolve() else {
            window.focus(&self.focus_handle, cx);
            return;
        };
        if let Some(p) = self.panes.get(&r.key) {
            let Some(tab) = p.active_tab() else {
                window.focus(&self.focus_handle, cx);
                return;
            };
            let handle = match &tab.content {
                TabContent::Terminal(t) => t.read(cx).focus_handle(cx),
                TabContent::File { item_id, .. } => {
                    if let Some(hosted) = self.service_workspaces.get(&r.key) {
                        let item_id = *item_id;
                        let item = hosted.read(cx).panes().iter().find_map(|pane| {
                            pane.read(cx)
                                .items()
                                .find(|i| i.item_id() == item_id)
                                .cloned()
                        });
                        if let Some(item) = item {
                            hosted.update(cx, |ws, cx| {
                                ws.activate_item(item.as_ref(), true, true, window, cx);
                            });
                            item.item_focus_handle(cx)
                        } else if hosted.read(cx).active_item(cx).is_some() {
                            hosted.read(cx).focus_handle(cx)
                        } else {
                            self.editor_host_focus.clone()
                        }
                    } else {
                        self.editor_host_focus.clone()
                    }
                }
                TabContent::Chat(c) => c.read(cx).focus_handle(cx),
                TabContent::Browser(page) => page.read(cx).activate_focus(cx),
                TabContent::Diff => self.diff_surface_focus.clone(),
                TabContent::ReviewOverview(_) => self.focus_handle.clone(),
                TabContent::Editor(ws) => {
                    if ws.read(cx).active_item(cx).is_some() {
                        ws.read(cx).focus_handle(cx)
                    } else {
                        self.editor_host_focus.clone()
                    }
                }
            };
            window.focus(&handle, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
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
                .map(|wp| {
                    wp.tabs
                        .iter()
                        .map(|t| crate::tmux::session_name(id, t.id))
                        .collect()
                })
                .unwrap_or_default();
            for name in names {
                crate::tmux::kill_session(&name);
            }
        }
        self.panes_state.remove(id);
        crate::panes_state::save(&self.panes_path, &mut self.panes_state);
        // The worktree's tabs are gone: so are their remote marks.
        self.remote_unmark(id, None);
    }

    /// Draw the next globally-unique, persisted tab id.
    pub(crate) fn next_tab_id(&mut self) -> u64 {
        // next_tab_id starts at 0 on a fresh install; first id handed out is 1.
        // Catch up with any other instance sharing the file first, so the id
        // (and the tmux session name built from it) is never handed out twice.
        let on_disk = crate::panes_state::load(&self.panes_path).next_tab_id;
        self.panes_state.next_tab_id = self.panes_state.next_tab_id.max(on_disk) + 1;
        let id = self.panes_state.next_tab_id;
        crate::panes_state::save(&self.panes_path, &mut self.panes_state);
        id
    }

    /// Rebuild the persisted tab records for the CURRENTLY-LIVE pane sets and
    /// write them. Worktrees not in `self.panes` right now (evicted, or not yet
    /// restored this session) keep their existing records, so lazy
    /// restore-on-reselect still finds them — eviction stays non-destructive.
    pub(crate) fn save_panes(&mut self, cx: &App) {
        use crate::panes_state::{TabRecord, WorktreePanes};
        // This save covers any browser change still waiting to be saved.
        self.browser_save = None;
        // Collect first to avoid borrowing self.panes and self.panes_state at once.
        let updates: Vec<(String, WorktreePanes)> = self
            .panes
            .iter()
            .map(|(key, pane)| {
                let tabs: Vec<TabRecord> = pane.tabs.iter().filter_map(|t| t.record(cx)).collect();
                let active = tabs
                    .iter()
                    .find(|t| t.id == pane.active)
                    .map(|t| t.id)
                    .or_else(|| tabs.first().map(|t| t.id));
                // A live agent tab names the pane's agent; with none (it came
                // back as a shell after a restart) keep the kind on record so
                // the pane can still show STOPPED and be restarted.
                let agent = pane.tabs.iter().find_map(|t| t.agent.clone()).or_else(|| {
                    self.panes_state
                        .worktrees
                        .get(key)
                        .and_then(|w| w.agent.clone())
                });
                (
                    key.clone(),
                    WorktreePanes {
                        active,
                        tabs,
                        agent,
                    },
                )
            })
            .collect();
        for (key, wp) in updates {
            self.panes_state.worktrees.insert(key, wp);
        }
        crate::panes_state::save(&self.panes_path, &mut self.panes_state);
    }

    /// Recreate a worktree's persisted tabs on first build. Live tmux sessions
    /// reattach; dead ones (reboot / killed) come back as a plain shell — never
    /// an unattended agent relaunch. Returns false if there was nothing to
    /// restore.
    pub(crate) fn restore_tabs(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use crate::panes_state::TabRecordKind as K;
        let saved = self.panes_state.worktrees.get(key);
        // Clone out before mutating self.
        let mut records: Vec<(u64, K, String)> = saved
            .map(|s| {
                s.tabs
                    .iter()
                    .map(|t| (t.id, t.kind.clone(), t.title.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let saved_active = saved.and_then(|s| s.active);
        let live = if self.has_tmux {
            crate::tmux::live_sessions()
        } else {
            Vec::new()
        };

        // Adopt this worktree's live tmux sessions the saved list lost track
        // of (another app instance overwrote panes.json, or we died before
        // saving). Their agents are still running with their context; without
        // this they sit detached and invisible while a fresh agent replaces
        // them.
        let known: Vec<u64> = records.iter().map(|r| r.0).collect();
        let orphans = crate::tmux::orphan_tab_ids(key, &live, &known);
        // Status written before this tab existed was dropped. Replay it once
        // the tab is attached, and only for these adopted sessions.
        let mut adopted_agents: Vec<(u64, AgentKind)> = Vec::new();
        if !orphans.is_empty() {
            let starts = crate::tmux::session_start_commands();
            for id in orphans {
                let name = crate::tmux::session_name(key, id);
                let agent = starts
                    .iter()
                    .find(|(n, _)| *n == name)
                    .and_then(|(_, cmd)| crate::tmux::agent_from_start_command(cmd));
                log::info!(
                    "adopting orphaned tmux session {name} ({:?})",
                    agent.as_ref().map(|a| a.1)
                );
                records.push(match agent {
                    Some((kind, prog)) => {
                        adopted_agents.push((id, kind.clone()));
                        (id, K::Agent(kind), prog.to_string())
                    }
                    None => (id, K::Shell, "❯_ shell".to_string()),
                });
                // Never hand this id out again.
                self.panes_state.next_tab_id = self.panes_state.next_tab_id.max(id);
            }
        }
        if records.is_empty() {
            return false;
        }

        for (id, kind, title) in records {
            match kind {
                K::Editor => self.open_tab(TabKind::Editor, window, cx),
                K::Chat => self.open_tab(TabKind::Chat, window, cx),
                K::Diff => self.open_tab(TabKind::Diff, window, cx),
                K::ReviewOverview(saved) => {
                    let mut overview = OverviewTab {
                        preferences: self.prefs.review_overview.clone(),
                        ..Default::default()
                    };
                    if let Some(saved) = saved {
                        overview.set_report(saved, cx);
                    }
                    self.attach_tab(
                        key,
                        Tab {
                            id,
                            title,
                            content: TabContent::ReviewOverview(overview),
                            agent: None,
                        },
                    );
                }
                // In place, with its saved id, like Review Overview. No
                // native view yet: `sync_browser_views` makes one if this
                // ends up the visible tab.
                K::Browser { url, title } => {
                    // Saved before `0.0.0.0`/`[::]` loaded through loopback.
                    let url = (!url.is_empty())
                        .then(|| crate::link_routing::loopback_for_unspecified(&url));
                    // Last session recorded its visit.
                    let page = self.new_browser_pane(
                        key,
                        url,
                        title,
                        crate::browser_pane::PageLoad::Restored,
                        window,
                        cx,
                    );
                    let title = page.read(cx).label();
                    self.attach_tab(
                        key,
                        Tab {
                            id,
                            title,
                            content: TabContent::Browser(page),
                            agent: None,
                        },
                    );
                }
                K::Shell => self.restore_terminal_tab(key, id, None, &title, &live, window, cx),
                K::Agent(a) => {
                    self.restore_terminal_tab(key, id, Some(a), &title, &live, window, cx)
                }
            }
        }

        // Restore the active tab if its id survived (ids are stable for
        // terminal/agent tabs; editor/chat drew fresh ids so may not match).
        if let Some(pane) = self.panes.get_mut(key) {
            let saved = saved_active.filter(|a| pane.tabs.iter().any(|t| t.id == *a));
            // No usable saved choice (e.g. tabs recovered from orphaned tmux
            // sessions): open on the oldest agent, the one holding the longest
            // conversation and the one A2A should deliver to, not whichever
            // tab happened to be rebuilt last.
            let pick = saved.or_else(|| {
                pane.tabs
                    .iter()
                    .filter(|t| t.agent.is_some())
                    .map(|t| t.id)
                    .min()
            });
            if let Some(active) = pick {
                pane.active = active;
                self.focus_active_pane(window, cx);
            }
        }
        let restored = self.panes.contains_key(key);
        if restored {
            self.replay_adopted_status(key, &adopted_agents, window, cx);
            self.save_panes(cx);
        }
        restored
    }

    /// Re-read status for sessions adopted after their first signal was dropped.
    /// A tab that is still closed stays rejected: nothing here invents one.
    fn replay_adopted_status(
        &mut self,
        key: &str,
        adopted: &[(u64, AgentKind)],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self
            .resolve()
            .filter(|r| r.key == key)
            .map(|r| r.path.clone())
        else {
            return;
        };
        for (id, kind) in adopted {
            let Some(signal) = saved_tab_signal(&root, *id, kind.clone()) else {
                continue;
            };
            self.apply_agent(signal, window.is_window_active(), cx);
        }
    }

    /// The review CLI is not installed. Drop the pending tab instead of
    /// opening a shell the report would never reach. The overview tab still
    /// holds the report.
    fn abandon_missing_review_cli(
        &mut self,
        key: &str,
        tab_id: u64,
        kind_label: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(pane) = self.panes.get_mut(key)
            && let Some(ix) = pane.tabs.iter().position(|t| t.id == tab_id)
        {
            pane.remove_tab_at(ix);
        }
        self.pending_agent_launches.remove(&tab_id);
        self.set_note(format!(
            "{kind_label} is not on PATH. The review was not opened in a terminal. The report is still in Review Overview."
        ));
        self.focus_active_pane(window, cx);
        self.save_panes(cx);
        cx.notify();
    }

    /// Rebuild one terminal/agent tab, preserving its persisted id. Live session
    /// → reattach (`attach_only`); dead → plain shell in the worktree.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore_terminal_tab(
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

        self.attach_tab(
            key,
            Tab {
                id,
                title: title.to_string(),
                content,
                agent: mark,
            },
        );
    }

    fn attach_tab(&mut self, key: &str, tab: Tab) {
        self.pane_clock = self.pane_clock.wrapping_add(1);
        let pane = self
            .panes
            .entry(key.to_string())
            .or_insert_with(|| PaneSet {
                tabs: Vec::new(),
                active: 0,
                last_used: 0,
            });
        pane.last_used = self.pane_clock;
        pane.active = tab.id;
        pane.tabs.push(tab);
    }
}

/// How long after a browser tab's last URL change its save runs.
const BROWSER_SAVE_DELAY: Duration = Duration::from_secs(1);

/// Whether opening a URL in a browser tab may land on a tab already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserOpen {
    /// An open from the app (a routed link, Ports' Open): focus the tab
    /// already on that URL, if any.
    ReuseSameUrl,
    /// A page's new-window request (`target=_blank`, `window.open`): always
    /// a tab of its own.
    AlwaysNew,
}

/// What a pane key names, for finding the repository it belongs to
/// ([`HarnessWorkspace::browser_project`]): the keys `resolve` gives panes.
#[derive(Debug, PartialEq, Eq)]
enum PaneOwner<'a> {
    /// `main:<repo name>`: a repository's main checkout.
    Main(&'a str),
    /// `task:<id>:member:<worktree>` (the coordinator's included) or the
    /// legacy `task:<id>`: a task.
    Task(&'a str),
    /// Anything else: a lone worktree's id.
    Worktree(&'a str),
}

fn pane_owner(key: &str) -> PaneOwner<'_> {
    if let Some(name) = key.strip_prefix("main:") {
        PaneOwner::Main(name)
    } else if let Some((task_id, _)) = a2a_broker::parse_task_member_pane_key(key) {
        PaneOwner::Task(task_id)
    } else if let Some(task_id) = a2a_broker::parse_legacy_task_pane_key(key) {
        PaneOwner::Task(task_id)
    } else {
        PaneOwner::Worktree(key)
    }
}

/// The tab an open of `url` lands on instead of a new one, if any: never
/// for a blank tab (`None`) or [`BrowserOpen::AlwaysNew`].
pub(crate) fn reusable_browser_tab(
    tabs: &[crate::panes_state::TabRecord],
    url: Option<&str>,
    how: BrowserOpen,
) -> Option<u64> {
    match how {
        BrowserOpen::ReuseSameUrl => find_browser_tab(tabs, url?),
        BrowserOpen::AlwaysNew => None,
    }
}

/// The browser tab among `tabs` showing `url`, compared as parsed URLs (so
/// `http://localhost:3000` matches `http://localhost:3000/`). A blank tab, or
/// an unparsable `url`, never matches.
pub(crate) fn find_browser_tab(tabs: &[crate::panes_state::TabRecord], url: &str) -> Option<u64> {
    use crate::panes_state::TabRecordKind as K;
    let comparable = |u: &str| url::Url::parse(u.trim()).ok().map(String::from);
    let want = comparable(url)?;
    tabs.iter().find_map(|t| match &t.kind {
        K::Browser { url, .. } if comparable(url).as_ref() == Some(&want) => Some(t.id),
        _ => None,
    })
}

/// The status file an adopted tab already wrote. `Unknown` and a missing
/// file stay quiet: there is nothing to replay.
fn saved_tab_signal(root: &Path, tab_id: u64, fallback: AgentKind) -> Option<AgentSignal> {
    let bytes = std::fs::read(agent_runtime::status_path(root, Some(tab_id))).ok()?;
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SavedTabStatus {
        state: AgentState,
        #[serde(default)]
        agent: Option<String>,
        #[serde(default)]
        session_file: Option<String>,
        #[serde(default)]
        tool: Option<agent_runtime::ToolCall>,
    }
    let saved: SavedTabStatus = serde_json::from_slice(&bytes).ok()?;
    if saved.state == AgentState::Unknown {
        return None;
    }
    let reason = match saved.state {
        AgentState::Waiting => agent_runtime::WaitReason::PermissionPrompt,
        AgentState::Idle => agent_runtime::WaitReason::TurnComplete,
        AgentState::Running | AgentState::Unknown => agent_runtime::WaitReason::None,
    };
    Some(AgentSignal {
        worktree_root: root.to_path_buf(),
        tab_id: Some(tab_id),
        kind: saved
            .agent
            .as_deref()
            .map(AgentKind::from)
            .unwrap_or(fallback),
        state: saved.state,
        reason,
        session_file: saved.session_file.map(PathBuf::from),
        tool: saved.tool,
    })
}

fn pane_to_evict<'a>(
    panes: &'a HashMap<String, PaneSet>,
    active: &str,
    pending_launches: &std::collections::HashSet<u64>,
) -> Option<&'a str> {
    panes
        .iter()
        .filter(|(key, pane)| {
            key.as_str() != active
                && !pane
                    .tabs
                    .iter()
                    .any(|tab| pending_launches.contains(&tab.id))
        })
        .min_by_key(|(_, pane)| pane.last_used)
        .map(|(key, _)| key.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_pane_key_names_its_checkout_task_or_worktree() {
        assert_eq!(pane_owner("main:omar-harness"), PaneOwner::Main("omar-harness"));
        assert_eq!(
            pane_owner(&a2a_broker::task_member_pane_key("auth", "omar-harness/auth")),
            PaneOwner::Task("auth")
        );
        assert_eq!(
            pane_owner(&a2a_broker::coordinator_pane_key("auth")),
            PaneOwner::Task("auth")
        );
        assert_eq!(pane_owner("task:auth"), PaneOwner::Task("auth"));
        assert_eq!(
            pane_owner("omar-harness/fix-login"),
            PaneOwner::Worktree("omar-harness/fix-login")
        );
    }

    #[test]
    fn closing_a_tab_selects_the_next_tab_then_the_previous() {
        for (ids, active, closing, expected) in [
            (&[1, 3, 2][..], 1, 0, 3),
            (&[1, 3, 2][..], 3, 1, 2),
            (&[1, 3, 2][..], 2, 2, 3),
            (&[1, 3, 2][..], 1, 1, 1),
            (&[1][..], 1, 0, 0),
        ] {
            let mut pane = PaneSet {
                tabs: ids
                    .iter()
                    .map(|&id| Tab {
                        id,
                        title: String::new(),
                        content: TabContent::Diff,
                        agent: None,
                    })
                    .collect(),
                active,
                last_used: 0,
            };

            pane.remove_tab_at(closing);

            assert_eq!(
                pane.active, expected,
                "closing tab {} in {ids:?}",
                ids[closing]
            );
        }
    }

    #[test]
    fn new_browser_tabs_land_after_the_active_tab() {
        let tab = |id| Tab {
            id,
            title: String::new(),
            content: TabContent::Diff,
            agent: None,
        };
        let mut pane = PaneSet {
            tabs: vec![tab(1), tab(2), tab(3)],
            active: 2,
            last_used: 0,
        };
        pane.insert_after_active(tab(9));
        let ids: Vec<u64> = pane.tabs.iter().map(|t| t.id).collect();
        assert_eq!(ids, [1, 2, 9, 3]);
        assert_eq!(pane.active, 9);
        // No active tab: at the end.
        pane.active = 0;
        pane.insert_after_active(tab(10));
        let ids: Vec<u64> = pane.tabs.iter().map(|t| t.id).collect();
        assert_eq!(ids, [1, 2, 9, 3, 10]);
    }

    #[test]
    fn find_browser_tab_matches_the_same_url_only() {
        use crate::panes_state::{TabRecord, TabRecordKind};
        let browser = |id: u64, url: &str| TabRecord {
            id,
            kind: TabRecordKind::Browser {
                url: url.into(),
                title: None,
            },
            title: "Browser".into(),
        };
        let tabs = vec![
            TabRecord {
                id: 1,
                kind: TabRecordKind::Shell,
                title: "Shell".into(),
            },
            browser(2, ""),
            browser(3, "http://localhost:3000/"),
            browser(4, "http://localhost:3000/docs"),
        ];
        // Equal after URL normalization (trailing slash, case of the host).
        assert_eq!(find_browser_tab(&tabs, "http://localhost:3000"), Some(3));
        assert_eq!(find_browser_tab(&tabs, "http://LOCALHOST:3000/"), Some(3));
        assert_eq!(find_browser_tab(&tabs, "http://localhost:3000/docs"), Some(4));
        // A different path, port or scheme is another page.
        assert_eq!(find_browser_tab(&tabs, "http://localhost:3000/other"), None);
        assert_eq!(find_browser_tab(&tabs, "http://localhost:3001/"), None);
        assert_eq!(find_browser_tab(&tabs, "https://localhost:3000/"), None);
        // A blank tab is never "the same page".
        assert_eq!(find_browser_tab(&tabs, ""), None);
        // App opens reuse the tab; a page's new window never does, even on
        // its own URL; a blank tab is always new.
        let url = Some("http://localhost:3000/");
        assert_eq!(reusable_browser_tab(&tabs, url, BrowserOpen::ReuseSameUrl), Some(3));
        assert_eq!(reusable_browser_tab(&tabs, url, BrowserOpen::AlwaysNew), None);
        assert_eq!(reusable_browser_tab(&tabs, None, BrowserOpen::ReuseSameUrl), None);
    }

    #[test]
    fn bulk_agent_launches_can_exceed_the_pane_cap_until_startup_finishes() {
        let mut pending = HashSet::new();
        let mut panes = HashMap::new();
        for id in 0..=MAX_LIVE_PANES as u64 {
            pending.insert(id);
            panes.insert(
                id.to_string(),
                PaneSet {
                    tabs: vec![Tab {
                        id,
                        title: String::new(),
                        content: TabContent::Diff,
                        agent: Some(AgentKind::OpenCode),
                    }],
                    active: id,
                    last_used: id,
                },
            );
        }

        assert!(panes.len() > MAX_LIVE_PANES);
        assert_eq!(pane_to_evict(&panes, "2", &pending), None);
        pending.remove(&2);
        assert_eq!(pane_to_evict(&panes, "2", &pending), None);
        pending.remove(&1);
        assert_eq!(pane_to_evict(&panes, "2", &pending), Some("1"));
        pending.remove(&0);
        assert_eq!(pane_to_evict(&panes, "2", &pending), Some("0"));
    }

    #[test]
    fn a_saved_tab_status_becomes_a_signal_and_unknown_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let path = agent_runtime::status_path(root, Some(7));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"state":"waiting","agent":"codex","sessionFile":"/tmp/s.jsonl","tool":{"name":"shell"}}"#,
        )
        .unwrap();
        let signal = saved_tab_signal(root, 7, AgentKind::ClaudeCode).unwrap();
        assert_eq!(signal.tab_id, Some(7));
        assert_eq!(signal.kind, AgentKind::Codex);
        assert_eq!(signal.state, AgentState::Waiting);
        assert_eq!(signal.reason, agent_runtime::WaitReason::PermissionPrompt);
        assert_eq!(
            signal.session_file.as_deref(),
            Some(Path::new("/tmp/s.jsonl"))
        );
        assert_eq!(signal.tool.unwrap().name, "shell");

        std::fs::write(&path, r#"{"state":"unknown"}"#).unwrap();
        assert!(saved_tab_signal(root, 7, AgentKind::Codex).is_none());
        assert!(saved_tab_signal(root, 8, AgentKind::Codex).is_none());

        std::fs::write(&path, r#"{"state":"idle"}"#).unwrap();
        let signal = saved_tab_signal(root, 7, AgentKind::Pi).unwrap();
        assert_eq!(signal.kind, AgentKind::Pi);
        assert_eq!(signal.reason, agent_runtime::WaitReason::TurnComplete);
    }
}
