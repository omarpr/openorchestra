//! The root `Render` impl that lays out the window, plus small shared
//! widgets (agent pill, CI summary, tool chips).

use super::*;

impl Focusable for HarnessWorkspace {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for HarnessWorkspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The font-size pref scales the whole app, not only the code
        // surfaces: chrome text is rem-based (text_xs/text_sm), so scaling
        // the window's rem size rescales it all — the same mechanism zed's
        // setup_ui_font uses inside the hosted workspace. 13px ↦ the 16px
        // default rem. Fixed px dimensions (bar heights, panel widths) stay
        // fixed on purpose; text is what legibility needs.
        let scale = self.prefs.font_size.unwrap_or(13.0) / 13.0;
        window.set_rem_size(px(16.0 * scale));

        // Keep the agent-command field focused while the Settings overlay edits
        // it. That field floats over the hosted zed editor, which holds window
        // focus when Settings is opened from the menu (⌘,) — so `begin_edit`
        // lands the caret but the editor behind keeps the keystrokes and the
        // field looks dead. The sidebar's rename uses this same input and never
        // sees this, because interacting with the sidebar already moved focus
        // off the editor. Re-assert only for this exact case, and only once
        // focus has actually escaped, so it converges instead of thrashing.
        if self.settings_open
            && matches!(
                self.edit_kind(),
                Some(EditKind::AgentCmd(_) | EditKind::RemotePort)
            )
            && let Some(editor) = self.edit_editor()
        {
            let handle = editor.focus_handle(cx);
            if !handle.is_focused(window) {
                window.focus(&handle, cx);
            }
        }

        let hx = theme(cx).clone();
        self.ensure_conductor(&hx, cx);
        self.ensure_indicator_ticker(cx);
        // The hosted zed Workspace's own actions (⌘P's ToggleFileFinder among
        // them) are attached here, at the root, not only around the File tab.
        // Since zed v1.2x `Workspace::render` no longer attaches them itself —
        // its MultiWorkspace wrapper does, and we host the Workspace without
        // one — so from a terminal, Review, or any non-file tab those actions
        // had no handler at all and ⌘P silently did nothing. The File tab's
        // inner wrapper still handles them first when it is focused.
        //
        // While the Editor tab is showing, the root must carry ITS actions,
        // not the service workspace's: `render_zed_modal` only draws the
        // Editor tab's finder then, so a root-level ⌘P (focus anywhere
        // outside the editor) used to open the service workspace's finder
        // invisibly — and that hidden finder then held focus, so ⌘P, ⌘C, ⌘V
        // and typing all looked dead until a click back into the text.
        let base = match self.active_ide_workspace().or_else(|| self.editor_workspace()) {
            Some(ws) => ws.update(cx, |w, cx| w.actions(div(), window, cx)),
            None => div(),
        };
        let mut root = base
            .relative()
            .key_context("Harness")
            .flex()
            .flex_col()
            .size_full()
            .bg(hx.bg0)
            .font_family(hx.font_ui)
            .text_color(hx.tx0)
            .child(self.render_top_bar(&hx, window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    // `h_full` alongside `flex_1`, the way zed's own containers
                    // pair them. flex_1 alone leaves the height *indefinite* in
                    // taffy's eyes: the div gets stretched to the right size,
                    // but percentage heights below it fall back to content
                    // size. zed's dock caches its panel as a childless leaf
                    // node, whose content height is zero — so the file tree
                    // laid out into a 239×0 box while every state probe read
                    // healthy. min_h_0 still allows shrinking.
                    .h_full()
                    .min_h_0()
                    // Collapsing hides the splitter too — a resize handle for
                    // a panel that isn't there is just a dead strip of chrome.
                    .when(self.prefs.sidebar_open, |d| {
                        d.child(self.render_sidebar(&hx, cx)).child(self.splitter(
                            &hx,
                            Drag::Sidebar,
                            cx,
                        ))
                    })
                    .when(!(self.prefs.diff_open && self.tools_maximized), |d| {
                        d.child(self.render_center(&hx, window, cx))
                    })
                    .when(self.prefs.diff_open, |d| {
                        d.when(!self.tools_maximized, |d| {
                            d.child(self.splitter(&hx, Drag::DiffPanel, cx))
                        })
                        .child(self.render_tool_panel(&hx, window, cx))
                    }),
            )
            .children(self.render_zed_modal(cx))
            .children(self.render_conductor(&hx, window, cx))
            .children(self.render_toast(&hx, cx))
            // The settings overlay is the LAST child on purpose: gpui paints
            // siblings in order, so an overlay added before the content row
            // renders underneath it — "settings don't open" while the state
            // toggles perfectly. Absolute positioning takes it out of flow;
            // being last takes it to the top.
            // Stacking order depends on whether the requirements screen is
            // *blocking*, and getting this wrong is what made Settings look
            // broken: an informational preflight rendered last covered a
            // Settings the user had deliberately opened, backdrop and all, so
            // ⌘, appeared to do nothing.
            //
            // A blocking screen must win over everything — there is nothing
            // else worth interacting with. A dismissible one must not swallow
            // a deliberate action; it waits underneath and is still there when
            // Settings closes, because it has not been acknowledged yet.
            .when(self.preflight_open && !crate::preflight::blocked(&self.preflight), |d| {
                d.child(self.render_preflight(&hx, cx))
            })
            .when(self.settings_open, |d| d.child(self.render_settings(&hx, cx)))
            .when(self.preflight_open && crate::preflight::blocked(&self.preflight), |d| {
                d.child(self.render_preflight(&hx, cx))
            })
            // Drag tracking lives on the root so the pointer can leave the
            // 6px handle mid-drag without the resize stopping.
            .on_action(
                cx.listener(|ws, _: &crate::CopyDiffSelection, _, cx| ws.copy_diff_selection(cx)),
            )
            .on_action(cx.listener(Self::focus_files_tool))
            .on_action(cx.listener(Self::focus_project_search))
            .on_action(cx.listener(Self::focus_git_tool))
            .on_action(cx.listener(Self::focus_review_tool))
            .on_action(cx.listener(|ws, _: &crate::NewTerminal, window, cx| {
                ws.open_tab(TabKind::Shell, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &crate::NewEditor, window, cx| {
                ws.open_tab(TabKind::Editor, window, cx)
            }))
            .on_action(cx.listener(|ws, _: &crate::CloseTab, window, cx| {
                log::debug!("CloseTab (root ⌘W)");
                let Some(r) = ws.resolve() else { return };
                let Some(active) = ws.panes.get(&r.key).map(|p| p.active) else {
                    return;
                };
                ws.request_close_tab(active, window, cx);
            }))
            .on_action(cx.listener(|_, _: &crate::About, window, cx| {
                crate::update_check::show_about(window, cx);
            }))
            .on_action(cx.listener(|_, _: &crate::CheckForUpdates, window, cx| {
                crate::update_check::check_for_updates(window, cx);
            }))
            .on_action(cx.listener(|ws, _: &crate::FirstTimeChecks, _, cx| {
                // Always re-probe on open: the point of reaching for it is
                // usually that something changed since startup.
                ws.preflight_open = true;
                ws.rerun_preflight(cx);
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &crate::OpenSettings, _, cx| {
                // Refuse while a required tool is missing: Settings sits behind
                // the blocking screen, so opening it would only mean a menu
                // item that visibly does nothing.
                if crate::preflight::blocked(&ws.preflight) {
                    return;
                }
                ws.settings_open = !ws.settings_open;
                if !ws.settings_open {
                    ws.remote_close_setup();
                }
                cx.notify();
            }))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab1, w, cx| ws.activate_tab_at(Some(0), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab2, w, cx| ws.activate_tab_at(Some(1), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab3, w, cx| ws.activate_tab_at(Some(2), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab4, w, cx| ws.activate_tab_at(Some(3), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab5, w, cx| ws.activate_tab_at(Some(4), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab6, w, cx| ws.activate_tab_at(Some(5), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab7, w, cx| ws.activate_tab_at(Some(6), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateTab8, w, cx| ws.activate_tab_at(Some(7), w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ActivateLastTab, w, cx| ws.activate_tab_at(None, w, cx)))
            .on_action(cx.listener(|ws, _: &crate::NextTab, w, cx| ws.cycle_tab(true, w, cx)))
            .on_action(cx.listener(|ws, _: &crate::PreviousTab, w, cx| ws.cycle_tab(false, w, cx)))
            .on_action(cx.listener(|ws, _: &crate::NextWorktree, w, cx| ws.cycle_worktree(true, w, cx)))
            .on_action(cx.listener(|ws, _: &crate::PreviousWorktree, w, cx| ws.cycle_worktree(false, w, cx)))
            .on_action(cx.listener(|ws, _: &crate::NextWaitingAgent, w, cx| ws.next_waiting_agent(w, cx)))
            .on_action(cx.listener(|ws, _: &crate::NewTask, w, cx| ws.open_new_task(w, cx)))
            .on_action(cx.listener(|ws, _: &crate::ToggleSidebar, _, cx| ws.toggle_sidebar(cx)))
            .on_action(cx.listener(|ws, _: &crate::ToggleTools, w, cx| ws.toggle_diff(w, cx)))
            .on_action(cx.listener(|ws, _: &crate::NewChat, window, cx| {
                ws.open_tab(TabKind::Chat, window, cx)
            }))
            // ⌘Q / menu quit routes through the keep-or-close-terminals prompt
            // instead of quitting outright. stop_propagation so the app-level
            // fallback `Quit` handler doesn't also fire and quit behind the
            // prompt's back.
            .on_action(cx.listener(|ws, _: &crate::Quit, _window, cx| {
                cx.stop_propagation();
                ws.request_quit(cx);
            }))
            .on_mouse_move(
                cx.listener(|ws, ev: &MouseMoveEvent, window, cx| ws.drag_move(ev, window, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseUpEvent, window, cx| {
                    ws.diff_selecting = false;
                    ws.conductor_drag_end(window, cx);
                    ws.end_drag(cx);
                }),
            );

        if let Some(m) = &self.menu {
            let menu = m.clone();
            root = root.child(
                div()
                    .id("menu-backdrop")
                    // Occlude, not just catch clicks: without it the elements under
                    // an overlay still hover, and their tooltips show through it.
                    .occlude()
                    .absolute()
                    .top(px(0.0))
                    .left(px(0.0))
                    .size_full()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _, _, cx| {
                            ws.menu = None;
                            cx.notify();
                        }),
                    ),
            );
            root = root.child(self.render_context_menu(&hx, &menu, cx));
        }

        if let Some((list, x, y)) = self.reorder_panel {
            root = root
                .child(
                    div()
                        .id("reorder-projects-backdrop")
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.reorder_panel = None;
                                cx.notify();
                            }),
                        ),
                )
                .child({
                    // A long list scrolls inside the panel instead of running
                    // off the window, and the whole panel stays on screen.
                    let win = window.viewport_size();
                    let list_max_h = (f32::from(win.height) * 0.6).min(420.0);
                    let rows = match list {
                        ReorderList::Projects => self.data.repos.len(),
                        ReorderList::Tasks => self.data.tasks.len(),
                    };
                    let (x, y) = reorder_panel_origin(
                        (f32::from(x), f32::from(y)),
                        (f32::from(win.width), f32::from(win.height)),
                        rows,
                        list_max_h,
                        f32::from(window.rem_size()),
                    );
                    self.render_reorder_panel(&hx, list, px(x), px(y), px(list_max_h), cx)
                });
        }

        if let Some((repo, x, y)) = self.project_menu.clone() {
            root = root
                .child(
                    div()
                        .id("project-menu-backdrop")
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.project_menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(self.render_project_menu(&hx, &repo, x, y, cx));
        }

        if let Some(draft) = self.comment_draft.clone() {
            root = root
                .child(
                    // Click outside saves and closes, so a comment is never
                    // lost by clicking away — the common accident.
                    div()
                        .id("comment-backdrop")
                        // Occlude, not just catch clicks: without it the elements under
                        // an overlay still hover, and their tooltips show through it.
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| ws.commit_comment(cx)),
                        ),
                )
                .child(self.render_comment_popover(&hx, &draft, window, cx));
        }

        if let Some(x) = self.tab_menu {
            root = root
                .child(
                    // Click-anywhere-else dismisses, same as the card menu.
                    div()
                        .id("tab-menu-backdrop")
                        // Occlude, not just catch clicks: without it the elements under
                        // an overlay still hover, and their tooltips show through it.
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.tab_menu = None;
                                cx.notify();
                            }),
                        ),
                )
                // Kept inside the window: the menu opens at the button, and
                // with many tabs (or a narrow window) that is close enough to
                // the right edge for the menu to be cut off.
                .child({
                    const MENU_W: f32 = 190.0;
                    let max_left = f32::from(window.viewport_size().width) - MENU_W - 8.0;
                    let x = px(f32::from(x).min(max_left).max(8.0));
                    self.render_tab_menu(&hx, x, cx)
                });
        }

        // A menu whose tab closed (or whose worktree is no longer selected)
        // has nothing to show: drop it rather than float an empty box.
        if let Some((tab_id, _)) = self.tab_context_menu
            && self.tab_context_item(tab_id).is_none()
        {
            self.tab_context_menu = None;
        }
        if let Some((tab_id, x)) = self.tab_context_menu {
            root = root
                .child(
                    div()
                        .id("tab-context-menu-backdrop")
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.tab_context_menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child({
                    const MENU_W: f32 = 220.0;
                    let max_left = f32::from(window.viewport_size().width) - MENU_W - 8.0;
                    let x = px(f32::from(x).min(max_left).max(8.0));
                    self.render_tab_context_menu(&hx, tab_id, x, cx)
                });
        }

        if let Some(at) = self.diff_base_menu {
            root = root
                .child(
                    // Click-anywhere-else dismisses, same as the card menu.
                    div()
                        .id("diff-base-backdrop")
                        // Occlude, not just catch clicks: without it the elements under
                        // an overlay still hover, and their tooltips show through it.
                        .occlude()
                        .absolute()
                        .top(px(0.0))
                        .left(px(0.0))
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|ws, _, _, cx| {
                                ws.diff_base_menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(self.render_diff_base_menu(&hx, at, cx));
        }

        if self.edit_prompt.is_some() {
            root = root.child(self.render_edit_prompt(&hx, cx));
        }

        if self.new_task.is_some() {
            self.sync_new_task_branch_placeholder(window, cx);
            root = root.child(self.render_new_task_dialog(&hx, cx));
        }

        if self.start_specialists.is_some() {
            root = root.child(self.render_start_specialists_dialog(&hx, cx));
        }

        if self.close_prompt.is_some() {
            root = root.child(self.render_close_prompt(&hx, cx));
        }
        if let Some(removal) = self.confirm_removal.clone() {
            root = root.child(self.render_removal_prompt(&hx, removal, cx));
        }
        if let Some(dialog) = self.render_end_confirm(&hx, cx) {
            root = root.child(dialog);
        }
        if let Some(dialog) = self.render_hook_trust(&hx, cx) {
            root = root.child(dialog);
        }
        root
    }
}


/// Repaint cadence for the sidebar indicators: ~15 fps is smooth for a 10px
/// spinner and a fraction of what a per-frame gpui animation would cost — the
/// whole chrome re-renders on every repaint, for as long as an agent works.
pub(crate) const INDICATOR_FRAME: Duration = Duration::from_millis(66);
/// One full spinner turn, and one breath of the waiting pulse.
pub(crate) const SPIN_PERIOD: f32 = 1.0;
pub(crate) const PULSE_PERIOD: f32 = 1.2;

impl HarnessWorkspace {
    /// Seconds on the indicators' shared clock, so every spinner turns in step.
    pub(crate) fn anim_secs(&self) -> f32 {
        self.anim_clock.elapsed().as_secs_f32()
    }

    /// Keep the sidebar indicators moving while any agent is running or
    /// waiting; stop the loop as soon as none is, so an idle app stays idle.
    pub(crate) fn ensure_indicator_ticker(&mut self, cx: &mut Context<Self>) {
        let animating = |ws: &Self| {
            ws.agent_status.values().any(|s| matches!(s, AgentState::Running | AgentState::Waiting))
                || !ws.ending.is_empty()
                // The sidebar's A2A connectors: a message dot or flash in
                // flight, or a delegation pulsing while it is worked on.
                || ws.a2a_links.is_animating(ws.anim_secs())
                || ws.a2a_links.any_active()
        };
        if self.indicator_ticking || !animating(self) {
            return;
        }
        self.indicator_ticking = true;
        cx.spawn(async move |view, cx| {
            loop {
                cx.background_executor().timer(INDICATOR_FRAME).await;
                let keep = view
                    .update(cx, |ws, cx| {
                        let keep = animating(ws);
                        ws.indicator_ticking = keep;
                        cx.notify();
                        keep
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        })
        .detach();
    }
}

/// In place of the agent pill while a row's worktree is being archived or
/// deleted: the same spinning arc, in grey, saying which.
pub(crate) fn busy_pill(hx: &HarnessTheme, label: &'static str, t: f32) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .bg(hx.surface2)
        .text_xs()
        .text_color(hx.tx1)
        .child(spin_arc(hx.tx1, tint(hx.tx2, 0.4), 10.0, t))
        .child(label)
}

/// The spinning arc every "working" indicator shares: `color` arc turning on a
/// faint full `track` ring, `size` px square. `t` is the shared animation
/// clock, so every spinner on screen turns in step.
pub(crate) fn spin_arc(color: gpui::Rgba, track: gpui::Rgba, size: f32, t: f32) -> gpui::Div {
    let angle = (t / SPIN_PERIOD).fract() * std::f32::consts::TAU;
    div()
        .flex_none()
        .relative()
        .size(px(size))
        .child(div().absolute().size_full().rounded_full().border_1().border_color(track))
        .child(
            gpui::svg()
                .absolute()
                .size_full()
                .path("icons/load_circle.svg")
                .text_color(color)
                .with_transformation(gpui::Transformation::rotate(gpui::radians(angle))),
        )
}

/// The agent state pill: a spinning accent arc while running, a breathing
/// amber dot while waiting on you. `t` is the shared animation clock in
/// seconds.
///
/// Nothing at all at rest. Nearly every row is idle nearly all the time, so an
/// IDLE pill on each one was noise that also made RUNNING and WAITING — the
/// states worth a glance — harder to spot.
pub(crate) fn agent_pill(hx: &HarnessTheme, state: AgentState, t: f32) -> AnyElement {
    let (color, label) = match state {
        AgentState::Running => (hx.accent, "RUNNING"),
        AgentState::Waiting => (hx.amber, "WAITING"),
        AgentState::Idle | AgentState::Unknown => return div().into_any_element(),
    };
    let bg = tint(color, 0.16);
    let size = px(10.0);
    let indicator = if state == AgentState::Running {
        spin_arc(color, tint(color, 0.3), 10.0, t)
    } else {
        // Waiting: eased breathing between 35% and 100%.
        let phase = (t / PULSE_PERIOD).fract() * std::f32::consts::TAU;
        let level = 0.35 + 0.65 * (0.5 - 0.5 * phase.cos());
        div()
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(7.0)).rounded_full().bg(color).opacity(level))
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .bg(bg)
        .text_xs()
        .text_color(color)
        .child(indicator)
        .child(label)
        .into_any_element()
}

/// A task agent that should be running but is not. Same shape as
/// [`agent_pill`], muted (it is a state to act on, not an alarm); the caller
/// makes it clickable to restart.
pub(crate) fn stopped_pill(hx: &HarnessTheme) -> gpui::Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_1()
        .px_1p5()
        .py_0p5()
        .rounded_full()
        .bg(tint(hx.tx2, 0.14))
        .text_xs()
        .text_color(hx.tx2)
        .child(div().size(px(7.0)).rounded_full().border_1().border_color(hx.tx2))
        .child("STOPPED")
}

/// The sidebar's row icons, as SVG asset paths. Real icons rather than text
/// glyphs: a `⌂` sits wherever the font's metrics put it, which was visibly
/// below the middle of its tile.
pub(crate) const ICON_MAIN_CHECKOUT: &str = "icons/oo/house.svg";
pub(crate) const ICON_WORKTREE: &str = "icons/git_branch.svg";

/// A sidebar row's leading icon tile (main checkout, worktree), coloured
/// by its agent: grey at rest, accent while running, amber with a breathing
/// glow while it waits on you. Colour only where it says something — at rest
/// every row is the same quiet grey, so the one that needs you stands out.
/// The glow breathes on the same clock and period as the WAITING pill's dot.
pub(crate) fn state_tile(hx: &HarnessTheme, icon: &'static str, state: AgentState, t: f32) -> gpui::Div {
    let (fg, tile) = match state {
        AgentState::Running => (hx.accent, div().bg(tint(hx.accent, 0.22)).border_color(tint(hx.accent, 0.5))),
        AgentState::Waiting => {
            let phase = (t / PULSE_PERIOD).fract() * std::f32::consts::TAU;
            let level = 0.35 + 0.65 * (0.5 - 0.5 * phase.cos());
            (
                hx.amber,
                div().bg(tint(hx.amber, 0.2)).border_color(tint(hx.amber, 0.6)).shadow(vec![gpui::BoxShadow {
                    color: tint(hx.amber, 0.45 * level).into(),
                    offset: gpui::point(px(0.0), px(0.0)),
                    blur_radius: px(10.0),
                    spread_radius: px(0.0),
                    inset: false,
                }]),
            )
        }
        // Fill and outline come from the text colour, not a surface step: in
        // themes whose `surface2` is close to the sidebar's own colour, the
        // resting tile vanished and left a lone glyph.
        AgentState::Idle | AgentState::Unknown => {
            (hx.tx1, div().bg(tint(hx.tx2, 0.12)).border_color(tint(hx.tx2, 0.35)))
        }
    };
    tile.size(px(24.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .child(gpui::svg().size(px(14.0)).flex_none().path(icon).text_color(fg))
}

/// The fill a selected sidebar row gets. An accent wash rather than the
/// neutral `surface` step plus a 1px accent edge: that pair barely showed
/// against the sidebar, and the edge read as part of the tree's guide line.
pub(crate) fn selected_row_bg(hx: &HarnessTheme) -> Rgba {
    tint(hx.accent, 0.2)
}

/// The hover fill for a selectable sidebar row: a faint accent wash, or a
/// stronger one on the selected row. Hover paints over the selected fill, and
/// the pointer is still on a row just after it's clicked — with a neutral
/// hover, the click showed grey instead of the selection and looked like it
/// had done nothing.
pub(crate) fn row_hover_bg(hx: &HarnessTheme, selected: bool) -> Rgba {
    tint(hx.accent, if selected { 0.26 } else { 0.09 })
}

/// A compact chip for the row's Git counts. Zero counts take no space.
pub(crate) fn git_counts(hx: &HarnessTheme, ahead: usize, behind: usize, dirty: usize) -> Option<gpui::Div> {
    let mut row = sidebar_chip(hx, hx.tx1).gap_1();
    for (count, icon, color, label) in [
        (ahead, "icons/arrow_up.svg", hx.green, "Commits ahead"),
        (behind, "icons/arrow_down.svg", hx.amber, "Commits behind"),
        (dirty, "icons/file_diff.svg", hx.amber, "Changed files"),
    ] {
        if count == 0 {
            continue;
        }
        row = row.child(
            div()
                .id(label)
                .flex()
                .items_center()
                .gap(px(2.0))
                .text_color(color)
                .tooltip(tip(format!("{label} · {count}")))
                .child(svg().path(icon).size(px(11.0)).text_color(color))
                .child(count.to_string()),
        );
    }
    (ahead > 0 || behind > 0 || dirty > 0).then_some(row)
}

pub(crate) fn sidebar_chip(hx: &HarnessTheme, color: Rgba) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.0))
        .rounded_md()
        .px_1()
        .line_height(gpui::rems(0.95))
        .text_size(gpui::rems(0.68))
        .font_family(hx.font_mono)
        .whitespace_nowrap()
        .text_color(color)
        .bg(tint(color, 0.13))
}

pub(crate) fn ci_summary(hx: &HarnessTheme, checks: &[forge::CheckRun]) -> Option<(Rgba, &'static str)> {
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        Some((hx.red, "Checks failed"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Running) {
        Some((hx.amber, "Checks in progress"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Pass) {
        Some((hx.green, "Checks passed"))
    } else {
        None
    }
}


/// How long a status toast stays up.
pub(crate) const NOTE_TTL: Duration = Duration::from_secs(6);

/// Whether a status message reports something going wrong.
fn is_failure(note: &str) -> bool {
    let n = note.to_ascii_lowercase();
    ["fail", "error", "could not", "couldn't", "cannot", "can't", "not trusted", "skipped"]
        .iter()
        .any(|w| n.contains(w))
}

impl HarnessWorkspace {
    /// Show a status message as a toast. Replaces the old always-on bottom
    /// bar, which kept the last message on screen forever.
    pub(crate) fn set_note(&mut self, note: String) {
        self.note = note;
        self.note_at = (!self.note.is_empty()).then(std::time::Instant::now);
    }

    /// Clear a toast that has been up for [`NOTE_TTL`]. Returns whether it did.
    pub(crate) fn expire_note(&mut self) -> bool {
        if self.note_at.is_some_and(|t| t.elapsed() >= NOTE_TTL) {
            self.note.clear();
            self.note_at = None;
            return true;
        }
        false
    }

    /// The current status message, bottom-centre; click to dismiss.
    fn render_toast(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.note_at?;
        if self.note.is_empty() {
            return None;
        }
        let failed = is_failure(&self.note);
        Some(
            div()
                .absolute()
                .bottom(px(20.0))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(
                    div()
                        .id("toast")
                        .occlude()
                        .max_w(px(640.0))
                        .px_3()
                        .py_1p5()
                        .rounded_md()
                        .bg(hx.surface2)
                        .border_1()
                        .border_color(if failed { tint(hx.red, 0.6) } else { hx.border2 })
                        .shadow_lg()
                        .text_sm()
                        .text_color(if failed { hx.red } else { hx.tx0 })
                        .cursor_pointer()
                        .tooltip(tip("Dismiss"))
                        .on_click(cx.listener(|ws, _, _, cx| {
                            ws.note.clear();
                            ws.note_at = None;
                            cx.notify();
                        }))
                        .child(self.note.clone()),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod toast_tests {
    use super::is_failure;

    #[test]
    fn failures_are_told_apart_from_progress() {
        assert!(is_failure("push failed: rejected"));
        assert!(is_failure("could not trust hooks in /x"));
        assert!(is_failure("setup skipped: hooks not approved"));
        assert!(!is_failure("archived ‘login’"));
        assert!(!is_failure("running setup… (see shell)"));
    }
}
