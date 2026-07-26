//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::*;

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
                return Some(crate::tmux::session_prefix(&w.id));
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
                attributables.push(Attributable {
                    label: w.name.clone(),
                    session_prefix: crate::tmux::session_prefix(&w.id),
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
        if !self.prefs.pet_enabled {
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
        let entry = self
            .manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == signal.worktree_root);
        let (name, repo) = match entry {
            Some(e) => (e.name.clone(), e.repo.clone()),
            None => (String::new(), String::new()),
        };
        let id = self.worktree_id_for_path(&signal.worktree_root).unwrap_or_default();
        self.show_pet(
            format!("{} needs your attention!", agent_display(&signal.kind)),
            cx,
        );
        crate::notifier::notify_agent(
            &agent_display(&signal.kind),
            &crate::notifier::label_for(&name, &signal.worktree_root),
            &repo,
            &id,
            crate::notifier::AgentAlert::NeedsAttention,
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
        let entry = self
            .manager
            .registry
            .entries()
            .iter()
            .find(|e| e.path == signal.worktree_root);
        let (name, repo) = match entry {
            Some(e) => (e.name.clone(), e.repo.clone()),
            None => (String::new(), String::new()),
        };
        let id = self.worktree_id_for_path(&signal.worktree_root).unwrap_or_default();
        self.show_pet(
            format!("{} finished — your turn", agent_display(&signal.kind)),
            cx,
        );
        crate::notifier::notify_agent(
            &agent_display(&signal.kind),
            &crate::notifier::label_for(&name, &signal.worktree_root),
            &repo,
            &id,
            crate::notifier::AgentAlert::Finished,
        );
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
            agent_kind: None,
            // Main's diff is its *uncommitted* work only (`git diff HEAD`), not
            // the branch-vs-default review a worktree card shows. The main
            // checkout often sits on a feature branch whose commits are all
            // landed; diffing against the default branch then paints a huge diff
            // over an already-committed tree, which reads as a bug. HEAD keeps
            // the panel empty once everything is committed. (Ahead/behind badges
            // still use RepoView.compare_ref — this only steers the diff.)
            compare_ref: "HEAD".to_string(),
            branch: r.head_branch.clone(),
        };
        match &self.selection {
            Selection::Main(repo) => {
                let r = self.data.repos.iter().find(|x| &x.name == repo).unwrap_or(primary);
                Some(main_of(r))
            }
            Selection::Worktree(id) => {
                for r in &self.data.repos {
                    if let Some(w) = r.worktrees.iter().find(|w| &w.id == id) {
                        return Some(Resolved {
                            key: id.clone(),
                            path: w.path.clone(),
                            agent_kind: Some(
                                self.manager
                                    .registry
                                    .get(id)
                                    .map(|e| e.agent_kind.clone())
                                    .unwrap_or_default(),
                            ),
                            compare_ref: r.compare_ref.clone(),
                            branch: w.branch.clone(),
                        });
                    }
                }
                Some(main_of(primary))
            }
        }
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
        if let Some(cmd) = self
            .prefs
            .agents
            .get(kind.as_str())
            .and_then(|p| p.command.clone())
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            return Some(agent_runtime::AgentCommand {
                program: shell.into(),
                args: vec!["-lc".into(), cmd],
                env: runtime_env,
                label: kind.as_str().to_string(),
            });
        }
        if runtime_cmd.is_none() {
            log::info!("{kind} not on PATH — falling back to a plain shell");
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
                });
                (
                    TabContent::Terminal(cx.new(|cx| {
                        TerminalPane::new(r.path.clone(), command, session, window, cx)
                    })),
                    label,
                )
            }
            TabKind::Editor => match crate::workspace_host::workspace_for(&r.path, window, cx) {
                Ok(hosted) => {
                    crate::workspace_host::probe(&hosted, window, cx);
                    // Our render tree hosts the modal overlay, so our render
                    // must re-run when the workspace's modal state changes —
                    // a child view's notify does not re-render the parent.
                    cx.observe(&hosted, |_, _, cx| cx.notify()).detach();
                    (TabContent::Editor(hosted), "{ } editor".to_string())
                }
                Err(e) => {
                    // Never fatal: the other tabs still work.
                    log::warn!("could not open project for {}: {e}", r.path.display());
                    self.note = format!("could not open project: {e}");
                    cx.notify();
                    return;
                }
            },
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
        self.focus_active_pane(window, cx);
        self.save_panes();
        cx.notify();
    }

    /// Close a tab, dropping whatever it held — a terminal's pty and its
    /// process, or a chat's `pi --mode rpc` sidecar.
    pub(crate) fn close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else { return };
        // Is the tab being closed an agent tab? If so, killing its session ends
        // that agent, and a dead agent never writes the resolving "idle" — so its
        // pill would stay stuck on WAITING/RUNNING forever. Clear it here.
        let was_agent = self
            .panes
            .get(&r.key)
            .and_then(|p| p.tabs.iter().find(|t| t.id == id))
            .is_some_and(|t| t.agent.is_some());
        if self.has_tmux {
            // Explicit close tears the session down (only app-exit detaches).
            crate::tmux::kill_session(&crate::tmux::session_name(&r.key, id));
        }
        if was_agent {
            // Unknown (renders as IDLE) rather than Idle: we killed the process,
            // we didn't observe a clean finish. A file event can't unstick this
            // because the dead agent will never touch status.json again.
            self.agent_status.insert(r.path.clone(), AgentState::Unknown);
        }
        let Some(pane) = self.panes.get_mut(&r.key) else { return };
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
                // With a file open, focus the workspace so the cursor lands in
                // the buffer. With none, focus our host wrapper so ⌘P / ⌘⇧F
                // still resolve — the empty pane would leave focus with nothing
                // carrying the "Workspace" context.
                TabContent::Editor(ws) => {
                    if ws.read(cx).active_item(cx).is_some() {
                        ws.read(cx).focus_handle(cx)
                    } else {
                        self.editor_host_focus.clone()
                    }
                }
                TabContent::Chat(c) => c.read(cx).focus_handle(cx),
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
        let rows = readable_diff_lines(&self.unified_for_file(&file));
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
        let abs = r.path.join(file);
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
        self.md_preview = None;
        if let Some(r) = self.resolve() {
            // Keep the live watch pointed at whatever the diff panel shows.
            self.diff_watch.watch(&r.path);
        }
        self.spawn_diff_compute(cx);
    }

    /// Recompute the diff for the *same* selection, in response to a live file
    /// change. Deliberately does **not** clear `expanded`/`md_preview`: the user
    /// is reviewing the same worktree, and resetting their expand/preview state
    /// on every agent edit would make review impossible.
    pub(crate) fn refresh_diff_live(&mut self, cx: &mut Context<Self>) {
        self.spawn_diff_compute(cx);
    }

    /// Spawn the off-thread git diff, discarding a result whose selection the
    /// user has since left (the `diff_gen` staleness guard).
    pub(crate) fn spawn_diff_compute(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.resolve() else {
            self.diff = worktrees::Diff::default();
            self.diff_pending = false;
            cx.notify();
            return;
        };

        self.diff_gen = self.diff_gen.wrapping_add(1);
        let generation = self.diff_gen;
        let (path, compare_ref) = (r.path.clone(), r.compare_ref.clone());
        self.diff_pending = true;

        cx.spawn(async move |view, cx| {
            let computed = cx
                .background_spawn(async move {
                    compute_diff(&GitWorktrees::new(SystemRunner), &path, &compare_ref)
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                // Stale result: the user moved on while git was working.
                if ws.diff_gen != generation {
                    return;
                }
                ws.diff = computed;
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
                let tabs = pane
                    .tabs
                    .iter()
                    .map(|t| TabRecord { id: t.id, kind: t.kind_tag(), title: t.title.clone() })
                    .collect();
                (key.clone(), WorktreePanes { active: Some(pane.active), tabs })
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
            .then(|| crate::terminal_pane::TerminalSession { name, attach_only: alive });
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
