//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    pub(crate) fn render_top_bar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let r = self.resolve();
        let branch = r.as_ref().map(|r| r.branch.clone()).unwrap_or_default();
        let (project, task) = self.location_names();
        // The left toggle shows/hides the whole left panel (stats + worktrees),
        // so it sits on the left and is named for the panel, not its list.
        let sidebar_toggle = div()
            .id("toggle-sidebar")
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .text_color(if self.prefs.sidebar_open { hx.tx1 } else { hx.tx2 })
            .cursor_pointer()
            .tooltip(tip("Toggle sidebar (⌘B)"))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("▤ sidebar")
            .on_click(cx.listener(|ws, _, _, cx| ws.toggle_sidebar(cx)));

        // Tools panel visibility. Always available so Files/Git stay reachable
        // even when Review lives in a center tab.
        let tools_toggle = div()
            .id("toggle-diff")
            .flex_none()
            .px_2()
            .py_0p5()
            .rounded_md()
            .text_xs()
            .text_color(if self.prefs.diff_open { hx.tx1 } else { hx.tx2 })
            .cursor_pointer()
            .tooltip(tip("Toggle tools panel: Files / Git / Review (⌘⇧B)"))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("tools ▤")
            .on_click(cx.listener(|ws, _, window, cx| ws.toggle_diff(window, cx)));

        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(38.0))
            .px_3()
            .bg(hx.bg0)
            .border_b_1()
            .border_color(hx.border)
            // Left: sidebar toggle. Center: title. Right: tools panel toggle.
            .child(sidebar_toggle)
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .min_w_0()
                    // Where you are, most specific last: project › task, then
                    // the branch. The app's own name was here before, and
                    // the window title already says that.
                    .child(div().flex_none().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(project))
                    .when_some(task, |d, task| {
                        d.child(div().flex_none().text_sm().text_color(hx.tx2).child("›")).child(
                            div()
                                .min_w_0()
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_sm()
                                .text_color(hx.tx0)
                                .child(task),
                        )
                    })
                    .child(div().flex_none().text_xs().text_color(hx.tx2).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(branch),
                    )
                    .children(self.render_a2a_delegation_chip(hx)),
            )
            .child(tools_toggle)
    }

    /// The task chat strip: one chip per repo agent of the selected
    /// multi-project task. HOME (the orchestrator, where the user talks about
    /// the whole task) comes first; each specialist shows its agent state and
    /// how many A2A delegations it is working on. Clicking a chip looks in on
    /// that agent. `None` for anything that isn't a multi-project task.
    pub(crate) fn render_task_strip(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Selection::Task { task_id, .. } = &self.selection else {
            return None;
        };
        let task = self.data.tasks.iter().find(|t| &t.id == task_id)?;
        if task.members.len() < 2 {
            return None;
        }
        let focused = self.resolve().map(|r| r.path);
        let delegations = self.a2a_delegation_summary();
        let t = self.anim_secs();

        let mut members: Vec<_> = task.members.iter().collect();
        members.sort_by_key(|m| !m.is_home);

        let mut strip = div()
            .id("task-strip")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(30.0))
            .px_3()
            .bg(hx.bg0)
            .border_b_1()
            .border_color(hx.border)
            .overflow_x_scroll()
            .child(
                div()
                    .flex_none()
                    .mr_1()
                    .text_size(px(10.0))
                    .text_color(hx.tx2)
                    .child("TASK AGENTS"),
            );
        for m in members {
            let selected = focused.as_ref() == Some(&m.path);
            let state = self.agent_status.get(&m.path).copied().unwrap_or(AgentState::Unknown);
            let active = delegations
                .iter()
                .filter(|(_, tenant, s)| tenant == &m.worktree_id && (s == "working" || s == "submitted"))
                .count();
            let label = if m.is_home {
                format!("★ {} · orchestrator", m.repo)
            } else {
                m.repo.clone()
            };
            let tooltip = if m.is_home {
                "HOME: chat here about the whole task — it delegates to the other repos over A2A"
            } else {
                "Specialist: works only in this repo; look in on its agent"
            };
            let sel = Selection::Task {
                task_id: task.id.clone(),
                // HOME is the task itself, so it selects the task, not a member.
                member_id: (!m.is_home).then(|| m.worktree_id.clone()),
            };
            strip = strip.child(
                div()
                    .id(gpui::SharedString::from(format!("task-strip-{}", m.worktree_id)))
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_1p5()
                    .pl_2()
                    .pr_1()
                    .py_0p5()
                    .rounded_md()
                    .border_1()
                    .border_color(if selected { hx.accent } else { hx.border })
                    .when(selected, |d| d.bg(hx.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .tooltip(tip(tooltip))
                    .on_click(cx.listener(move |ws, _, window, cx| ws.select(sel.clone(), window, cx)))
                    .child(
                        div()
                            .text_xs()
                            .text_color(if m.is_home { hx.accent } else if selected { hx.tx0 } else { hx.tx1 })
                            .child(label),
                    )
                    .when(active > 0, |d| {
                        d.child(
                            div()
                                .text_size(px(10.0))
                                .font_family(hx.font_mono)
                                .text_color(hx.amber)
                                .child(format!("{active} A2A")),
                        )
                    })
                    .child(agent_pill(hx, state, t)),
            );
        }
        Some(strip.into_any_element())
    }

    /// Compact A2A delegation status for the active multi-member task.
    fn render_a2a_delegation_chip(&self, hx: &HarnessTheme) -> Option<AnyElement> {
        let rows = self.a2a_delegation_summary();
        if rows.is_empty() {
            let r = self.resolve()?;
            let tid = r.task_id.as_ref()?;
            if !self.task_brokers.contains_key(tid) {
                return None;
            }
            return Some(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("A2A")
                    .into_any_element(),
            );
        }
        let working = rows
            .iter()
            .filter(|(_, _, s)| *s == "working" || *s == "submitted")
            .count();
        let done = rows.iter().filter(|(_, _, s)| *s == "completed").count();
        let label = format!("A2A {working} active · {done} done");
        Some(
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_1p5()
                .py_0p5()
                .rounded_md()
                .bg(hx.surface)
                .text_xs()
                .font_family(hx.font_mono)
                .text_color(if working > 0 { hx.amber } else { hx.tx2 })
                .child(label)
                .into_any_element(),
        )
    }

    /// The selected project's name and, for a task, the task's name (with the
    /// focused member's project when the task spans several).
    pub(crate) fn location_names(&self) -> (String, Option<String>) {
        match &self.selection {
            Selection::Main(repo) if !repo.is_empty() => (repo.clone(), None),
            Selection::Main(_) => ("No project".into(), None),
            Selection::Task { task_id, member_id } => {
                let Some(task) = self.task_store.get(task_id) else {
                    return ("No project".into(), None);
                };
                let member = member_id
                    .as_deref()
                    .and_then(|id| task.members.iter().find(|m| m.worktree_id == id))
                    .or_else(|| task.home_member());
                let project = member.map(|m| m.repo.clone()).unwrap_or_default();
                (project, Some(task.name.clone()))
            }
            Selection::Worktree(id) => match self.manager.registry.get(id) {
                Some(e) => (e.repo.clone(), Some(e.name.clone())),
                None => ("No project".into(), None),
            },
        }
    }
}
