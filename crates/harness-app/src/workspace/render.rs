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
            && matches!(self.edit_kind(), Some(EditKind::AgentCmd(_)))
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
            .child(self.render_top_bar(&hx, cx))
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
                ws.close_tab(active, window, cx);
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
            let menu = CardMenu {
                id: m.id.clone(),
                x: m.x,
                y: m.y,
            };
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
    let angle = (t / SPIN_PERIOD).fract() * std::f32::consts::TAU;
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
        .child(
            div()
                .relative()
                .size(px(10.0))
                .child(div().absolute().size_full().rounded_full().border_1().border_color(tint(hx.tx2, 0.4)))
                .child(
                    gpui::svg()
                        .absolute()
                        .size_full()
                        .path("icons/load_circle.svg")
                        .text_color(hx.tx1)
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(angle))),
                ),
        )
        .child(label)
}

/// The agent state pill: a spinning accent arc while running, a breathing
/// amber dot while waiting on you, a still grey ring at rest. `t` is the
/// shared animation clock in seconds.
pub(crate) fn agent_pill(hx: &HarnessTheme, state: AgentState, t: f32) -> impl IntoElement {
    let (color, label) = match state {
        AgentState::Running => (hx.accent, "RUNNING"),
        AgentState::Waiting => (hx.amber, "WAITING"),
        AgentState::Idle | AgentState::Unknown => (hx.tx2, "IDLE"),
    };
    let bg = if matches!(state, AgentState::Idle | AgentState::Unknown) {
        hx.surface2
    } else {
        tint(color, 0.16)
    };
    let size = px(10.0);
    let indicator = match state {
        AgentState::Running => {
            let angle = (t / SPIN_PERIOD).fract() * std::f32::consts::TAU;
            div()
                .relative()
                .size(size)
                // A faint full ring as the track the arc runs on.
                .child(
                    div()
                        .absolute()
                        .size_full()
                        .rounded_full()
                        .border_1()
                        .border_color(tint(color, 0.3)),
                )
                .child(
                    gpui::svg()
                        .absolute()
                        .size_full()
                        .path("icons/load_circle.svg")
                        .text_color(color)
                        .with_transformation(gpui::Transformation::rotate(gpui::radians(angle))),
                )
        }
        AgentState::Waiting => {
            // Eased breathing between 35% and 100%.
            let phase = (t / PULSE_PERIOD).fract() * std::f32::consts::TAU;
            let level = 0.35 + 0.65 * (0.5 - 0.5 * phase.cos());
            div()
                .size(size)
                .flex()
                .items_center()
                .justify_center()
                .child(div().size(px(7.0)).rounded_full().bg(color).opacity(level))
        }
        AgentState::Idle | AgentState::Unknown => div()
            .size(size)
            .flex()
            .items_center()
            .justify_center()
            .child(div().size(px(7.0)).rounded_full().border_1().border_color(color)),
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
}

pub(crate) fn ci_summary(hx: &HarnessTheme, checks: &[forge::CheckRun]) -> Option<(Rgba, &'static str)> {
    if checks.is_empty() {
        return None;
    }
    if checks.iter().any(|c| c.status == CheckStatus::Fail) {
        Some((hx.red, "ci"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Running) {
        Some((hx.amber, "ci"))
    } else if checks.iter().any(|c| c.status == CheckStatus::Pass) {
        Some((hx.green, "ci"))
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
