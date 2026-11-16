//! The center tab strip: opening, closing, moving and focusing tabs, and
//! persisting/restoring them (with their tmux sessions) across restarts.

use super::*;

impl HarnessWorkspace {
    /// Open a new tab in the active worktree's set and focus it.
    pub(crate) fn open_tab(&mut self, kind: TabKind, window: &mut Window, cx: &mut Context<Self>) {
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
                    self.set_note("could not open the editor for this worktree".to_string());
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
        let Some(hosted) = self.ensure_service_workspace(window, cx) else {
            self.set_note("could not open an editor".to_string());
            cx.notify();
            return;
        };
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
        crate::workspace_host::open_file_at(&hosted, &root, &rel, line, false, window, cx);
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
    /// Close a tab from the UI. Closing a task pane's agent tab stops one of
    /// the task's agents, so it asks first; everything else closes directly.
    pub(crate) fn request_close_tab(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
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
        let task_name = self.task_store.get(&tid).map(|t| t.name.clone()).unwrap_or(tid);
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
        let Some(r) = self.resolve() else { return };
        let key = r.key.clone();
        let file_item = self.panes.get(&key).and_then(|p| {
            p.tabs.iter().find(|t| t.id == id).and_then(|t| match &t.content {
                TabContent::File { item_id, .. } => Some(*item_id),
                _ => None,
            })
        });
        if let Some(item_id) = file_item
            && let Some(hosted) = self.service_workspaces.get(&key).cloned() {
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
        let was_agent = self
            .panes
            .get(&key)
            .and_then(|p| p.tabs.iter().find(|t| t.id == id))
            .is_some_and(|t| t.agent.is_some());
        if self.has_tmux {
            // Explicit close tears the session down (only app-exit detaches).
            crate::tmux::kill_session(&crate::tmux::session_name(&key, id));
        }
        // A closed tab is never reachable from a phone again.
        self.remote_unmark(&key, Some(id));
        if was_agent {
            // Unknown (renders as IDLE) rather than Idle: we killed the process,
            // we didn't observe a clean finish. A file event can't unstick this
            // because the dead agent will never touch status.json again.
            self.conductor.stopped(&r.path);
            self.agent_since.insert(r.path.clone(), (std::time::SystemTime::now(), None));
            self.agent_status.insert(r.path.clone(), AgentState::Unknown);
        }
        let Some(pane) = self.panes.get_mut(&key) else { return };
        let Some(ix) = pane.tabs.iter().position(|t| t.id == id) else { return };
        pane.tabs.remove(ix);
        // The pane keeps its agent on record even when its last agent tab is
        // closed on purpose: a task needs every agent, so a closed one shows
        // STOPPED with a Restart like one lost to a computer restart.
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
                // A live agent tab names the pane's agent; with none (it came
                // back as a shell after a restart) keep the kind on record so
                // the pane can still show STOPPED and be restarted.
                let agent = pane
                    .tabs
                    .iter()
                    .find_map(|t| t.agent.clone())
                    .or_else(|| self.panes_state.worktrees.get(key).and_then(|w| w.agent.clone()));
                (key.clone(), WorktreePanes { active, tabs, agent })
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
    pub(crate) fn restore_tabs(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        use crate::panes_state::TabRecordKind as K;
        let saved = self.panes_state.worktrees.get(key);
        // Clone out before mutating self.
        let mut records: Vec<(u64, K, String)> = saved
            .map(|s| s.tabs.iter().map(|t| (t.id, t.kind.clone(), t.title.clone())).collect())
            .unwrap_or_default();
        let saved_active = saved.and_then(|s| s.active);
        let live = if self.has_tmux { crate::tmux::live_sessions() } else { Vec::new() };

        // Adopt this worktree's live tmux sessions the saved list lost track
        // of (another app instance overwrote panes.json, or we died before
        // saving). Their agents are still running with their context; without
        // this they sit detached and invisible while a fresh agent replaces
        // them.
        let known: Vec<u64> = records.iter().map(|r| r.0).collect();
        let orphans = crate::tmux::orphan_tab_ids(key, &live, &known);
        if !orphans.is_empty() {
            let starts = crate::tmux::session_start_commands();
            for id in orphans {
                let name = crate::tmux::session_name(key, id);
                let agent = starts
                    .iter()
                    .find(|(n, _)| *n == name)
                    .and_then(|(_, cmd)| crate::tmux::agent_from_start_command(cmd));
                log::info!("adopting orphaned tmux session {name} ({:?})", agent.as_ref().map(|a| a.1));
                records.push(match agent {
                    Some((kind, prog)) => (id, K::Agent(kind), prog.to_string()),
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
                K::Shell => self.restore_terminal_tab(key, id, None, &title, &live, window, cx),
                K::Agent(a) => self.restore_terminal_tab(key, id, Some(a), &title, &live, window, cx),
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
                pane.tabs.iter().filter(|t| t.agent.is_some()).map(|t| t.id).min()
            });
            if let Some(active) = pick {
                pane.active = active;
                self.focus_active_pane(window, cx);
            }
        }
        let restored = self.panes.contains_key(key);
        if restored {
            self.save_panes();
        }
        restored
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
