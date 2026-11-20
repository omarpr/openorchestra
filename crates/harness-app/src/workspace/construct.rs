//! `HarnessWorkspace::new`: load state from `~/.openorchestra`, start the
//! watchers, pollers and background loops, and restore the last session.

use super::*;

impl HarnessWorkspace {
    pub fn new(launch_hint: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let git = GitWorktrees::new(SystemRunner);

        // App-level state dir, independent of any repo.
        let data_dir = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| launch_hint.clone())
            .join(".openorchestra");
        if let Err(e) = crate::state_files::secure_dir(&data_dir) {
            log::warn!("could not restrict {}: {e}", data_dir.display());
        }
        let repos_path = data_dir.join("repos.json");
        let archived_path = data_dir.join("archived.json");
        let archived_repos = load_repos(&archived_path);
        let prefs_path = data_dir.join("prefs.json");
        let prefs = load_prefs(&prefs_path);
        crate::link_routing::set_routing_enabled(prefs.links_in_browser_tab);
        let reg_path = data_dir.join("registry.json");
        let panes_path = data_dir.join("panes.json");
        let mut panes_state = crate::panes_state::load(&panes_path);
        let has_tmux = crate::tmux::has_tmux();

        let mut repo_roots = load_repos(&repos_path);
        // Auto-add the launch dir if it's inside a git repo.
        if let Ok(root) = git.repo_root(&launch_hint)
            && !repo_roots.contains(&root)
        {
            repo_roots.push(root);
            save_repos(&repos_path, &repo_roots);
        }

        let mut registry = Registry::load(&reg_path).unwrap_or_else(|e| {
            log::warn!("registry load failed ({e}); starting empty");
            Registry::new_at(&reg_path)
        });
        let tasks_path = data_dir.join("tasks.json");
        let mut task_store = TaskStore::load(&tasks_path).unwrap_or_else(|e| {
            log::warn!("tasks load failed ({e}); starting empty");
            TaskStore::new_at(&tasks_path)
        });
        match migrate_worktrees_to_tasks(&mut registry, &mut task_store) {
            Ok(n) if n > 0 => log::info!("migrated {n} worktrees to single-member tasks"),
            Ok(_) => {}
            Err(e) => log::warn!("worktree→task migration failed: {e}"),
        }
        {
            let homes: Vec<(String, String)> = task_store
                .active()
                .filter_map(|t| {
                    t.home_member()
                        .map(|m| (t.id.clone(), m.worktree_id.clone()))
                })
                .collect();
            let n = crate::panes_state::migrate_task_pane_keys(&mut panes_state, &homes);
            if n > 0 {
                crate::panes_state::save(&panes_path, &mut panes_state);
                log::info!("migrated {n} legacy task pane keys to per-member keys");
            }
        }
        let manager = Manager::new(registry, git, data_dir.join("worktrees"));
        let data = WorkspaceData::build(
            manager.git(),
            &manager.registry,
            &task_store,
            &repo_roots,
            None,
        );

        let (poller, forge_rx) = Poller::spawn(
            Arc::new(GhCli::new()),
            data.forge_branches(),
            PollerConfig::default(),
        );

        // One bus for every adapter's signal (§4.6). The sidebar pill, the
        // status bar, and the notifier each take their own subscription, so it
        // must fan out rather than work-steal.
        let bus = Bus::new();
        let status_rx = Some(bus.subscribe());
        let status_source = match StatusFileSource::new(bus.clone()) {
            Ok(mut source) => {
                for e in manager.registry.active() {
                    if let Err(err) = source.watch(&e.path, e.agent_kind.clone()) {
                        log::warn!("cannot watch {}: {err}", e.path.display());
                    }
                }
                // Main checkouts are first-class (D8 revised): watch each repo
                // root too, so an agent run on `main` drives the same status
                // pill and notifications a worktree does. The kind is a fallback
                // — status files self-identify their writer.
                for root in &repo_roots {
                    if let Err(err) = source.watch(root, AgentKind::default()) {
                        log::warn!("cannot watch main {}: {err}", root.display());
                    }
                }
                // A multi-project task's coordinator runs in the task folder,
                // not a worktree: watch it so its pill works from launch.
                let tasks_root = data_dir.join("tasks");
                for task in task_store
                    .active()
                    .filter(|t| super::coordinator::has_coordinator(t))
                {
                    let dir = worktrees::task_coordinator_dir(&tasks_root, &task.slug);
                    if let Err(err) = source.watch(&dir, task.agent_kind.clone()) {
                        log::warn!("cannot watch coordinator {}: {err}", dir.display());
                    }
                }
                Some(source)
            }
            Err(e) => {
                log::warn!("status watcher unavailable: {e}");
                None
            }
        };

        let (diff_watch, diff_dirty_rx) = crate::diff_watch::DiffWatcher::new();

        // Watched from the start, and retargeted by `refresh()` whenever the
        // repo list changes. Every repo is watched at once, unlike the diff
        // watch: a card for a repo you are not looking at is still on screen,
        // and a badge is only worth showing if it is current.
        let (mut git_watch, git_moved_rx) = crate::git_watch::GitWatcher::new();
        git_watch.watch_repos(&repo_roots);

        let selection = data
            .repos
            .first()
            .map(|r| Selection::Main(r.name.clone()))
            .unwrap_or_else(|| Selection::Main(String::new()));
        let new_target_repo = data
            .repos
            .first()
            .map(|r| r.name.clone())
            .unwrap_or_default();
        let new_task_repos = if new_target_repo.is_empty() {
            Vec::new()
        } else {
            vec![new_target_repo.clone()]
        };
        let new_task_home = new_target_repo.clone();

        let (a2a_change_tx, a2a_change_rx) = crossbeam_channel::bounded::<()>(1);
        let (a2a_inbox_tx, a2a_inbox_rx) =
            crossbeam_channel::unbounded::<a2a_broker::InboxDelivery>();

        let mut this = Self {
            manager,
            task_store,
            data_dir: data_dir.clone(),
            data,
            repo_roots,
            repos_path,
            selection,
            panes: HashMap::new(),
            service_workspaces: HashMap::new(),
            ide_workspaces: HashMap::new(),
            finder_dismiss_sub: None,
            liveness_pending: false,
            preview_slot: None,
            tab_scroll: gpui::ScrollHandle::new(),
            tab_scrolled_to: std::cell::Cell::new(0),
            files_sub_tab: FilesSubTab::Explorer,
            project_search: None,
            diff: worktrees::Diff::default(),
            unified_diff: UnifiedDiff::default(),
            new_task_repos,
            new_task_home,
            expanded: std::collections::HashSet::new(),
            diff_focus: None,
            hooks_edit: None,
            diff_selection: None,
            diff_selecting: false,
            review: crate::diff_review::ReviewNotes::load(
                &crate::diff_review::ReviewNotes::path_in(&data_dir),
            ),
            review_path: crate::diff_review::ReviewNotes::path_in(&data_dir),
            comment_draft: None,
            clearing_review: false,
            settings_open: false,
            settings_focus: ui::ModalFocus::new(cx),
            settings_section: SettingsSection::General,
            review_model_catalogs: Default::default(),
            // Probed once at startup; the screen opens below if this machine is
            // missing something that blocks, or if it has never been shown.
            preflight_open: false,
            preflight: Vec::new(),
            conductor: Default::default(),
            conductor_view: None,
            conductor_frames: None,
            conductor_frames_accent: None,
            conductor_ticking: false,
            conductor_hovered: false,
            conductor_drag: None,
            anim_clock: std::time::Instant::now(),
            indicator_ticking: false,
            titlebar_should_move: false,
            rebinding: None,
            md_preview: None,
            diff_gen: 0,
            diff_pending: false,
            pane_clock: 0,
            pending_agent_launches: Default::default(),
            panes_state,
            panes_path,
            has_tmux,
            edit: None,
            new_task: None,
            start_specialists: None,
            task_brokers: HashMap::new(),
            a2a_change_tx,
            a2a_inbox_tx,
            a2a_pending_inject: HashMap::new(),
            missing_agents: Default::default(),
            restart_tried: Default::default(),
            continue_next_agent: false,
            a2a_links: Default::default(),
            new_target_repo,
            menu: None,
            project_menu: None,
            reorder_panel: None,
            tab_menu: None,
            tab_context_menu: None,
            diff_base_menu: None,
            browser_window_hidden: false,
            browser_save: None,
            close_prompt: None,
            edit_prompt: None,
            ending: HashMap::new(),
            end_confirm: None,
            hook_trust: None,
            note: String::new(),
            note_at: None,
            forge_status: HashMap::new(),
            agent_status: HashMap::new(),
            remote: super::remote::RemoteRuntime::new(&data_dir),
            _poller: Some(poller),
            status_source,
            diff_watch,
            git_watch,
            status_pending: false,
            status_rerun: false,
            tools_maximized: false,
            push_state: None,
            push_armed: false,
            pushing: false,
            push_probe_pending: false,
            row_cache: Default::default(),
            state_dir: data_dir.clone(),
            prefs,
            prefs_path,
            drag: None,
            bus,
            focus_handle: cx.focus_handle(),
            editor_host_focus: cx.focus_handle(),
            diff_surface_focus: cx.focus_handle(),
            diff_files_scroll: gpui::ScrollHandle::new(),
            resource: None,
            resource_history: crate::resource_monitor::History::new(RESOURCE_HISTORY),
            ports_sample: None,
            ports_seq_issued: 0,
            ports_seq_applied: 0,
            ports_gate: Default::default(),
            ports_sample_at: None,
            ports_scan_failed: false,
            ports_sort_menu: None,
            ports_sort_trigger_press: None,
            archived_repos,
            archived_path,
            confirm_removal: None,
        };
        // Appearance before panes: restore_tabs may open an Editor, which
        // constructs a hosted Workspace that observes window appearance and
        // reloads GlobalTheme from ThemeSettings. Sync ThemeSettings first so
        // that reload cannot swap the editor to One Light while chrome is dark.
        this.apply_appearance(cx);
        this.ensure_pane(window, cx);
        // What this machine can actually do, probed off the UI thread so startup
        // does not stall waiting on up to nine CLI `--version` subprocesses.
        // Opens when something Required is missing (and then it cannot be
        // dismissed), or once on a first ever launch — but never for an optional
        // gap alone, because a screen that appears every morning because
        // rust-analyzer is absent is a screen nobody reads.
        cx.spawn(async move |view, cx| {
            let checks = cx
                .background_spawn(async move {
                    crate::preflight::probe(crate::preflight::version_on_path)
                })
                .await;
            view.update(cx, |ws, cx| {
                let short: Vec<&str> = checks
                    .iter()
                    .filter(|c| c.missing())
                    .map(|c| c.name)
                    .collect();
                let blocked = crate::preflight::blocked(&checks);
                log::info!(
                    "preflight: missing {short:?}, blocked={blocked}, seen_before={}",
                    ws.prefs.seen_preflight
                );
                ws.preflight = checks;
                if blocked || !ws.prefs.seen_preflight {
                    ws.preflight_open = true;
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        // The restored selection is set directly above, not through `select`,
        // so nothing created its Files/Git host: a window that reopened on the
        // Files or Git tab showed "Open editor" until something else forced it.
        // Do here what `select` does for every later selection.
        let _ = this.ensure_service_workspace(window, cx);
        // The cards were built without `git status` (see `WorkspaceData::build`);
        // fill their badges in off-thread.
        this.refresh_status_live(cx);
        if this.prefs.diff_open
            && matches!(this.prefs.tool_tab, ToolTab::Files | ToolTab::Git)
            && this.files_sub_tab != FilesSubTab::Search
        {
            this.activate_hosted_tool_panel(this.prefs.tool_tab, window, cx);
        }
        this.recompute_diff(cx);
        // Phone remote: watch remote.json and start serving if it's enabled.
        this.remote_init(cx);
        // If no project/pane claimed focus (e.g. empty startup state), focus the
        // workspace root so global shortcuts and menu dispatch have an active node.
        if window.focused(cx).is_none() {
            window.focus(&this.focus_handle, cx);
        }

        // Browser tabs drop their native view while the window is minimized
        // or the app hidden (memory; the page reloads when shown). gpui
        // draws no frames for a hidden window, so `render`'s reconcile can't
        // see this; the visibility observer can.
        cx.observe_window_visibility(window, |ws, visibility, window, cx| {
            ws.window_visibility_changed(visibility, window, cx);
        })
        .detach();

        // Route the window's close button (red X) through the same
        // keep-or-close-terminals prompt as ⌘Q. `request_quit` returns whether
        // the app may close now: `false` holds the window open while the prompt
        // is up; `true` when there's nothing to ask (no sessions, or a
        // remembered choice) and the quit has already begun.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_window, cx| {
            weak.update(cx, |ws, cx| ws.request_quit(cx))
                .unwrap_or(true)
        });

        // Notification "View" clicks: the platform delegate pushes worktree
        // ids; select them and front the app. Foreground actions activate the
        // app at the OS level already — cx.activate makes the dev-fallback
        // path behave the same.
        let nav_rx = crate::notifier::nav_rx();
        cx.spawn_in(window, async move |view, cx| {
            loop {
                match nav_rx.try_recv() {
                    Ok(id) => {
                        let ok = view.update_in(cx, |ws, window, cx| {
                            if !id.is_empty() {
                                ws.navigate_to_alert(&id, window, cx);
                            }
                            cx.activate(true);
                        });
                        if ok.is_err() {
                            break;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        })
        .detach();

        cx.spawn(async move |view, cx| {
            loop {
                match forge_rx.try_recv() {
                    Ok(update) => {
                        if view
                            .update(cx, |ws, cx| ws.apply_forge(update, cx))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) if e.is_empty() => {
                        cx.background_executor()
                            .timer(Duration::from_millis(400))
                            .await
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();

        // A2A broker task-state changes: the delegation chip in the chrome
        // and the sidebar's connector lines.
        cx.spawn(async move |view, cx| {
            loop {
                match a2a_change_rx.try_recv() {
                    Ok(()) => {
                        while a2a_change_rx.try_recv().is_ok() {}
                        if view
                            .update(cx, |ws, cx| {
                                ws.observe_a2a_links();
                                cx.notify()
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        })
        .detach();

        // A2A inbox → specialist agent tmux inject.
        cx.spawn(async move |view, cx| {
            loop {
                match a2a_inbox_rx.try_recv() {
                    Ok(delivery) => {
                        if view
                            .update(cx, |ws, _cx| ws.deliver_a2a_inbox(delivery))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(Duration::from_millis(200))
                            .await;
                    }
                    Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                }
            }
        })
        .detach();

        if let Some(status_rx) = status_rx {
            cx.spawn_in(window, async move |view, cx| {
                loop {
                    match status_rx.try_recv() {
                        Ok(ev) => {
                            if view
                                .update_in(cx, |ws, window, cx| {
                                    ws.apply_agent(ev, window.is_window_active(), cx);
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(e) if e.is_empty() => {
                            cx.background_executor()
                                .timer(Duration::from_millis(400))
                                .await
                        }
                        Err(_) => break,
                    }
                }
            })
            .detach();
        }

        // Live diff refresh: a file changed under the selected root. The 250 ms
        // poll cadence is the debounce — a burst of edits in one agent turn
        // collapses to one recompute per tick, and the `diff_gen` guard drops
        // any overlap. State-preserving so an in-progress review is not reset.
        cx.spawn(async move |view, cx| {
            loop {
                match diff_dirty_rx.try_recv() {
                    Ok(_) => {
                        // Coalesce everything already queued this instant.
                        while diff_dirty_rx.try_recv().is_ok() {}
                        // The same edit that moved the diff moved the selected
                        // card's `~N`. Nothing else sees a plain unstaged write —
                        // it touches no git file — so this signal is the only way
                        // that badge tracks typing.
                        if view
                            .update(cx, |ws, cx| {
                                ws.refresh_diff_live(cx);
                                ws.refresh_status_live(cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) if e.is_empty() => {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();

        // Live sidebar badges: git moved under some repo's `.git`. Same drain
        // shape as the diff watch, and the 250 ms cadence is again the debounce
        // — a commit writes the index, a ref and often packed-refs, which is one
        // recompute here rather than three.
        cx.spawn(async move |view, cx| {
            loop {
                match git_moved_rx.try_recv() {
                    Ok(path) => {
                        log::debug!("git moved: {}", path.display());
                        let remote_updates: std::collections::HashSet<_> = std::iter::once(path)
                            .chain(git_moved_rx.try_iter())
                            .filter_map(|path| {
                                crate::git_watch::remote_tracking_repo(&path).map(Path::to_owned)
                            })
                            .collect();
                        if view
                            .update(cx, |ws, cx| {
                                // Cheap when nothing changed (it compares first).
                                // A repo's first `git worktree add` creates
                                // `.git/worktrees`, which needs a watch of its own.
                                let roots = ws.repo_roots.clone();
                                ws.git_watch.watch_repos(&roots);
                                if !remote_updates.is_empty() {
                                    for root in roots.iter().filter(|root| {
                                        remote_updates.contains(*root)
                                            || std::fs::canonicalize(root)
                                                .is_ok_and(|path| remote_updates.contains(&path))
                                    }) {
                                        ws.refresh_forge_repo(root);
                                    }
                                    cx.notify();
                                }
                                ws.refresh_status_live(cx);
                                // A commit or a push moves what the push control
                                // offers, and both land here.
                                ws.refresh_push_state(cx);
                                // And what the diff shows. The diff watcher skips
                                // `.git` by design, so a commit in the selected
                                // worktree — which moves HEAD and empties the
                                // working-tree diff without touching one tracked
                                // file — reached nothing: the panel kept listing
                                // files that were already committed. A checkout or
                                // a rebase is the same story with more files.
                                ws.refresh_diff_live(cx);
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(e) if e.is_empty() => {
                        cx.background_executor()
                            .timer(Duration::from_millis(250))
                            .await
                    }
                    Err(_) => break,
                }
            }
        })
        .detach();

        // Toast expiry: a cheap once-a-second check that repaints only when a
        // message actually times out.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if view
                    .update(cx, |ws, cx| {
                        if ws.expire_note() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Backstop. The watches above cover every git event and the selected
        // worktree's edits; what neither sees is an agent writing files in a
        // worktree you are not viewing, which changes that card's `~N` without
        // touching git. Ten seconds is slow enough to be free on an idle app —
        // the recompute runs off-thread and repaints only when a number moved.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(10))
                    .await;
                if view
                    .update(cx, |ws, cx| {
                        ws.refresh_status_live(cx);
                        ws.refresh_push_state(cx);
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Missing task agents. Which panes should run an agent is read on the
        // UI thread (cheap); asking tmux what is running happens off it.
        if has_tmux {
            cx.spawn(async move |view, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                    let Ok(expected) = view.update(cx, |ws, _| ws.task_panes_expecting_agents())
                    else {
                        break;
                    };
                    if expected.is_empty() {
                        continue;
                    }
                    let panes = cx
                        .background_spawn(async move { crate::tmux::pane_commands() })
                        .await;
                    let Some(panes) = panes else { continue };
                    let missing = crate::tmux::missing_agent_keys(&expected, &panes);
                    if view
                        .update(cx, |ws, cx| ws.set_missing_agents(missing, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }

        // Unstick agent pills. A status file left on waiting/running by a
        // process that died without writing "idle" (killed mid-dialog, crashed)
        // would show forever; every few seconds, drop any such pill whose pid is
        // gone. The probe is a couple of tiny file reads only when a non-terminal
        // pill exists, so an idle app does no work.
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(3)).await;
                if view
                    .update(cx, |ws, cx| ws.reconcile_agent_liveness(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Sample openOrchestra's own CPU/memory footprint for the sidebar band.
        // The sysinfo refresh + tmux pane read run on a background thread; only
        // the (cheap, pure) attribution runs on the UI thread. The `System` is
        // reused across ticks so CPU figures are real deltas, not zeros.
        let sampler_has_tmux = has_tmux;
        cx.spawn(async move |view, cx| {
            let mut sys = sysinfo::System::new();
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let (returned, raw) = cx
                    .background_spawn(async move {
                        let mut sys = sys;
                        let raw = crate::resource_monitor::sample_once(&mut sys, sampler_has_tmux);
                        (sys, raw)
                    })
                    .await;
                sys = returned;
                if view
                    .update(cx, |ws, cx| ws.apply_resource(raw, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        // Listening ports for the Ports tool tab. Polls only while that tab is
        // on screen; `set_tool_tab` takes an immediate sample when it opens.
        // The first pass runs without waiting, so a Ports tab restored open
        // at launch fills in at once rather than after a full interval.
        cx.spawn(async move |view, cx| {
            let mut first = true;
            loop {
                if !std::mem::take(&mut first) {
                    cx.background_executor().timer(Duration::from_secs(3)).await;
                }
                let Ok(visible) = view.read_with(cx, |ws, _| ws.ports_visible()) else {
                    break;
                };
                if !visible {
                    continue;
                }
                // A scan already running (a refresh): skip this tick.
                let Ok(started) = view.update(cx, |ws, _| {
                    ws.ports_gate.try_start(false).then(|| ws.next_ports_seq())
                }) else {
                    break;
                };
                let Some(seq) = started else {
                    // Still redraw: a sample can age past current meanwhile.
                    let _ = view.update(cx, |_, cx| cx.notify());
                    continue;
                };
                let started = std::time::Instant::now();
                let sample = cx
                    .background_spawn(async move { crate::ports::sample(sampler_has_tmux) })
                    .await;
                // `None`: the scan timed out; keep the last sample, retry next tick.
                if view
                    .update(cx, |ws, cx| ws.finish_ports_scan(seq, started, sample, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        this
    }
}
