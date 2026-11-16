//! Guided "Start agents" dialog after creating a multi-member task.
//!
//! Two questions: open an agent in every project? Then which enabled agent?

use super::super::*;

impl HarnessWorkspace {
    pub(crate) fn open_start_specialists(&mut self, task_id: String, cx: &mut Context<Self>) {
        self.start_specialists = Some(StartSpecialistsDialog {
            task_id,
            step: StartSpecialistsStep::AskOpenAll,
        });
        cx.notify();
    }

    pub(crate) fn close_start_specialists(&mut self, cx: &mut Context<Self>) {
        self.start_specialists = None;
        cx.notify();
    }

    pub(crate) fn start_specialists_choose_open_all(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.start_specialists.as_mut() else {
            return;
        };
        let selected = default_enabled_agent(&self.prefs)
            .unwrap_or(AgentKind::OpenCode);
        dialog.step = StartSpecialistsStep::PickAgent { selected };
        cx.notify();
    }

    pub(crate) fn start_specialists_select_agent(
        &mut self,
        kind: AgentKind,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.start_specialists.as_mut() else {
            return;
        };
        if let StartSpecialistsStep::PickAgent { selected } = &mut dialog.step {
            *selected = kind;
        }
        cx.notify();
    }

    pub(crate) fn start_specialists_back(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.start_specialists.as_mut() else {
            return;
        };
        dialog.step = StartSpecialistsStep::AskOpenAll;
        cx.notify();
    }

    /// Persist the chosen agent on the task + member registry entries, then
    /// open an Agent tab in every member and finish on the coordinator.
    pub(crate) fn confirm_start_all_agents(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(dialog) = self.start_specialists.as_ref() else {
            return;
        };
        let StartSpecialistsStep::PickAgent { selected } = &dialog.step else {
            return;
        };
        let kind = selected.clone();
        let task_id = dialog.task_id.clone();

        if enabled_agents(&self.prefs).is_empty() {
            self.set_note("enable an agent in Settings first".to_string());
            cx.notify();
            return;
        }

        self.apply_task_agent_kind(&task_id, kind.clone());
        self.prefs.last_task_agent = Some(kind.as_str().to_string());
        self.save_prefs(cx);

        let (member_ids, coordinated) = match self.task_store.get(&task_id) {
            Some(task) => (
                task.members
                    .iter()
                    .map(|m| m.worktree_id.clone())
                    .collect::<Vec<_>>(),
                super::super::coordinator::has_coordinator(task),
            ),
            None => {
                self.close_start_specialists(cx);
                return;
            }
        };
        // Every repo is a specialist and none is special: open each one's
        // agent, then land on the coordinator, which delegates to them.
        for mid in member_ids {
            self.select(
                Selection::Task {
                    task_id: task_id.clone(),
                    member_id: Some(mid),
                },
                window,
                cx,
            );
            self.open_tab(TabKind::Agent(kind.clone()), window, cx);
        }
        if coordinated {
            self.select(
                Selection::Task {
                    task_id: task_id.clone(),
                    member_id: None,
                },
                window,
                cx,
            );
            // Task creation already selected the task, which opened the
            // coordinator's agent with the kind the task had then (the
            // default), before this pick. End with exactly one agent tab
            // running the picked kind: two live agents would make A2A inbox
            // injection ambiguous, and a stale kind ignores the user's pick.
            // Open first so closing the last wrong tab never falls back to
            // spawning a shell.
            let tabs: Vec<(u64, Option<AgentKind>)> = self
                .panes
                .get(&a2a_broker::coordinator_pane_key(&task_id))
                .map(|p| p.tabs.iter().map(|t| (t.id, t.agent.clone())).collect())
                .unwrap_or_default();
            let (open, close) = coordinator_agent_fixup(&tabs, &kind);
            if open {
                self.open_tab(TabKind::Agent(kind.clone()), window, cx);
            }
            // `close_tab` kills each tab's tmux session too.
            for id in close {
                self.close_tab(id, window, cx);
            }
        }
        self.close_start_specialists(cx);
    }

    fn apply_task_agent_kind(&mut self, task_id: &str, kind: AgentKind) {
        let member_ids: Vec<String> = match self.task_store.get_mut(task_id) {
            Some(task) => {
                task.agent_kind = kind.clone();
                task.members
                    .iter()
                    .map(|m| m.worktree_id.clone())
                    .collect()
            }
            None => return,
        };
        for mid in member_ids {
            if let Some(entry) = self.manager.registry.get_mut(&mid) {
                entry.agent_kind = kind.clone();
            }
        }
        let _ = self.task_store.save();
        let _ = self.manager.registry.save();
    }

    pub(crate) fn render_start_specialists_dialog(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(dialog) = self.start_specialists.as_ref() else {
            return div().into_any_element();
        };
        let Some(task) = self.task_store.get(&dialog.task_id) else {
            return div().into_any_element();
        };

        let card = match &dialog.step {
            StartSpecialistsStep::AskOpenAll => {
                self.render_start_ask_open_all(hx, task, cx)
                    .into_any_element()
            }
            StartSpecialistsStep::PickAgent { selected } => {
                self.render_start_pick_agent(hx, task, selected, cx)
                    .into_any_element()
            }
        };

        div()
            .id("start-specialists-backdrop")
            // Occlude, not just catch clicks: without it the elements under
            // an overlay still hover, and their tooltips show through it.
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _, cx| {
                    ws.close_start_specialists(cx);
                }),
            )
            .child(card)
            .into_any_element()
    }

    fn render_start_ask_open_all(
        &self,
        hx: &HarnessTheme,
        task: &worktrees::Task,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let n = task.members.len();
        let project_list = task
            .members
            .iter()
            .map(|m| m.repo.clone())
            .collect::<Vec<_>>()
            .join(", ");

        div()
            .id("start-specialists-card")
            .w(px(440.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child("Start agents"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(hx.tx2)
                    .child(format!(
                        "Open an agent in each of these {n} projects? Each gets its own cwd; the task's coordinator delegates to them.",
                    )),
            )
            .child(
                div()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.tx1)
                    .child(project_list),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        div()
                            .id("start-specialists-skip")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(34.0))
                            .rounded_md()
                            .border_1()
                            .border_color(hx.border2)
                            .bg(hx.surface)
                            .text_sm()
                            .text_color(hx.tx0)
                            .cursor_pointer()
                            .child("Skip")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.close_start_specialists(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("start-specialists-open-all")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(34.0))
                            .rounded_md()
                            .border_1()
                            .border_color(hx.accent)
                            .bg(tint(hx.accent, 0.16))
                            .text_sm()
                            .text_color(hx.accent)
                            .cursor_pointer()
                            .child("Open in all")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.start_specialists_choose_open_all(cx);
                            })),
                    ),
            )
    }

    fn render_start_pick_agent(
        &self,
        hx: &HarnessTheme,
        task: &worktrees::Task,
        selected: &AgentKind,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let enabled = enabled_agents(&self.prefs);
        let mut rows = div().flex().flex_col().gap_1();

        if enabled.is_empty() {
            rows = rows.child(
                div()
                    .text_sm()
                    .text_color(hx.tx2)
                    .child("No agents are enabled. Turn one on in Settings, then try again."),
            );
        } else {
            for kind in &enabled {
                let is_sel = kind == selected;
                let label = agent_display(kind);
                let kind_click = kind.clone();
                rows = rows.child(
                    div()
                        .id(gpui::SharedString::from(format!(
                            "start-agent-pick-{}",
                            kind.as_str()
                        )))
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .py_1p5()
                        .rounded_md()
                        .border_1()
                        .border_color(if is_sel { hx.accent } else { hx.border })
                        .bg(if is_sel {
                            tint(hx.accent, 0.12)
                        } else {
                            hx.surface
                        })
                        .cursor_pointer()
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            ws.start_specialists_select_agent(kind_click.clone(), cx);
                        }))
                        .child(div().size(px(18.0)).child(agent_icon(kind, 17.0)))
                        .child(
                            div()
                                .flex_1()
                                .text_sm()
                                .text_color(hx.tx0)
                                .child(label),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(if is_sel { hx.accent } else { hx.tx2 })
                                .child(if is_sel { "●" } else { "○" }),
                        ),
                );
            }
        }

        let n = task.members.len();
        let can_start = !enabled.is_empty();

        div()
            .id("start-specialists-card")
            .w(px(440.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx0)
                    .child("Choose agent"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(hx.tx2)
                    .child(format!(
                        "Same agent opens in all {n} projects. Only agents enabled in Settings are listed.",
                    )),
            )
            .child(rows)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        div()
                            .id("start-specialists-back")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(34.0))
                            .rounded_md()
                            .border_1()
                            .border_color(hx.border2)
                            .bg(hx.surface)
                            .text_sm()
                            .text_color(hx.tx0)
                            .cursor_pointer()
                            .child("Back")
                            .on_click(cx.listener(|ws, _, _, cx| {
                                ws.start_specialists_back(cx);
                            })),
                    )
                    .child(
                        div()
                            .id("start-specialists-confirm")
                            .flex_1()
                            .flex()
                            .items_center()
                            .justify_center()
                            .h(px(34.0))
                            .rounded_md()
                            .border_1()
                            .border_color(if can_start { hx.accent } else { hx.border })
                            .bg(if can_start {
                                tint(hx.accent, 0.16)
                            } else {
                                hx.surface
                            })
                            .text_sm()
                            .text_color(if can_start { hx.accent } else { hx.tx2 })
                            .cursor_pointer()
                            .child("Start agents")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                if can_start {
                                    ws.confirm_start_all_agents(window, cx);
                                }
                            })),
                    ),
            )
    }
}

/// What the coordinator pane needs so it ends with exactly one agent tab,
/// running `picked`: whether to open a `picked` agent, and which agent tabs
/// (by id) to close. `tabs` is the pane's tabs as `(id, agent kind)`.
///
/// Task creation selects the task, which opens the coordinator's agent with
/// the task's kind at that moment (the default); the dialog's pick comes
/// later, so a mismatched tab is replaced rather than kept beside the new one.
fn coordinator_agent_fixup(
    tabs: &[(u64, Option<AgentKind>)],
    picked: &AgentKind,
) -> (bool, Vec<u64>) {
    let keep = tabs
        .iter()
        .find(|(_, k)| k.as_ref() == Some(picked))
        .map(|(id, _)| *id);
    let close = tabs
        .iter()
        .filter(|(id, k)| k.is_some() && Some(*id) != keep)
        .map(|(id, _)| *id)
        .collect();
    (keep.is_none(), close)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_coordinator_agent_is_kept() {
        let tabs = [(1, None), (2, Some(AgentKind::ClaudeCode))];
        assert_eq!(coordinator_agent_fixup(&tabs, &AgentKind::ClaudeCode), (false, vec![]));
    }

    #[test]
    fn wrong_kind_is_replaced() {
        let tabs = [(1, None), (2, Some(AgentKind::ClaudeCode))];
        assert_eq!(coordinator_agent_fixup(&tabs, &AgentKind::Codex), (true, vec![2]));
    }

    #[test]
    fn no_agent_opens_one() {
        assert_eq!(coordinator_agent_fixup(&[(1, None)], &AgentKind::Codex), (true, vec![]));
    }

    #[test]
    fn extra_agents_are_closed_keeping_one_match() {
        let tabs = [
            (2, Some(AgentKind::ClaudeCode)),
            (3, Some(AgentKind::Codex)),
            (4, Some(AgentKind::Codex)),
        ];
        assert_eq!(coordinator_agent_fixup(&tabs, &AgentKind::Codex), (false, vec![2, 4]));
    }
}
