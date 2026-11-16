//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

mod projects;
pub(crate) mod links;

/// The sidebar's three top-level sections, in render order.
///
/// They exist as one type so every header goes through
/// [`HarnessWorkspace::section_header`] — height, separators, disclosure and
/// collapse state cannot drift apart the way three hand-rolled headers did.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SidebarSection {
    Stats,
    Tasks,
    Projects,
    Archived,
}

impl SidebarSection {
    /// Also the persistence key in `Prefs::collapsed_sections`.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Stats => "STATS",
            Self::Tasks => "TASKS",
            Self::Projects => "PROJECTS",
            Self::Archived => "ARCHIVED",
        }
    }

    /// Whether the section starts collapsed. ARCHIVED does: it is a list you
    /// reach into now and then, and open by default it grows with every
    /// archive and eats the height PROJECTS scrolls in.
    pub(crate) fn collapsed_by_default(self) -> bool {
        matches!(self, Self::Archived)
    }
}

/// The most of the sidebar's height TASKS takes before scrolling inside
/// itself, leaving PROJECTS room however many tasks there are.
const TASKS_MAX_SHARE: f32 = 0.3;

/// Archived rows shown before the section scrolls inside itself.
const ARCHIVED_VISIBLE_ROWS: f32 = 5.0;
const ARCHIVED_ROW_H: f32 = 30.0;

/// Every section header is this tall — the value TASKS was missing, which is
/// why it read as cramped next to STATS and PROJECTS.
const SECTION_HEADER_H: f32 = 34.0;

impl HarnessWorkspace {

    /// A section's trailing `+` button. Stops propagation so adding a task or
    /// project never also toggles the section it lives in.
    fn section_add_button(
        &self,
        hx: &HarnessTheme,
        id: &'static str,
        tip_text: &'static str,
        on_click: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .size(px(20.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(hx.tx1)
            .cursor_pointer()
            .tooltip(tip(tip_text))
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .child("+")
            .on_click(cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                on_click(ws, window, cx);
            }))
    }

    /// `collapsed_sections` records the sections the user has flipped away
    /// from their default, so for every section but ARCHIVED (default
    /// collapsed) membership still simply means "collapsed" — existing prefs
    /// files read exactly as before.
    pub(crate) fn section_collapsed(&self, section: SidebarSection) -> bool {
        self.prefs.collapsed_sections.contains(section.label()) != section.collapsed_by_default()
    }

    /// One sidebar section header: disclosure caret, label, optional trailing
    /// action. The whole strip is the click target.
    ///
    /// STATS is the only section without a top border — it sits directly under
    /// the window chrome, which already draws that edge. Every other section
    /// carries one so the sidebar reads as stacked bands.
    ///
    /// Only a collapsed header draws a bottom rule. An open section's content
    /// hangs straight off its header: a rule between them made PROJECTS read
    /// as an empty band with the projects in some unlabelled section below.
    fn section_header(
        &self,
        hx: &HarnessTheme,
        section: SidebarSection,
        action: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.section_collapsed(section);
        let label = section.label();
        div()
            .id(label)
            .flex()
            .flex_none()
            .items_center()
            .gap_2()
            .h(px(SECTION_HEADER_H))
            .px_3()
            .when(section != SidebarSection::Stats, |d| d.border_t_1())
            .when(collapsed, |d| d.border_b_1())
            .border_color(hx.border)
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .tooltip(tip(if collapsed {
                "Expand section"
            } else {
                "Collapse section"
            }))
            .on_click(cx.listener(move |ws, _, _, cx| {
                // Same set-not-map discipline as project collapse: going back
                // to the default removes the entry rather than persisting it.
                if !ws.prefs.collapsed_sections.remove(label) {
                    ws.prefs.collapsed_sections.insert(label.to_string());
                }
                ws.save_prefs(cx);
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child(if collapsed { "▸" } else { "▾" }),
            )
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx2)
                    .child(label),
            )
            .children(action)
    }

    /// The resource band atop the sidebar: openOrchestra's whole CPU/memory
    /// footprint as two labelled sparklines, with a per-worktree breakdown on
    /// hover. `base + Σ worktrees` by construction, so the hover rows add up to
    /// the totals shown here.
    pub(crate) fn render_resource_band(&self, hx: &HarnessTheme, _cx: &mut Context<Self>) -> impl IntoElement {
        use crate::resource_monitor::{fmt_cpu, fmt_mem};
        let snap = self.resource.as_ref();
        let cpu_label = snap.map(|s| fmt_cpu(s.total.cpu_pct)).unwrap_or_else(|| "—".into());
        let mem_label = snap.map(|s| fmt_mem(s.total.rss_bytes)).unwrap_or_else(|| "—".into());
        let cpu_hist: Vec<f32> = self.resource_history.iter().map(|u| u.cpu_pct).collect();
        let mem_hist: Vec<f32> = self.resource_history.iter().map(|u| u.rss_bytes as f32).collect();

        // Hover breakdown: worktrees with any usage, most-CPU first, then the
        // shared base-app row (muted) so the rows sum to the total.
        let mut rows: Vec<ResourceRow> = Vec::new();
        if let Some(s) = snap {
            let mut wts: Vec<&crate::resource_monitor::WorktreeUsage> = s
                .per_worktree
                .iter()
                .filter(|w| w.usage.cpu_pct >= 0.5 || w.usage.rss_bytes > 0)
                .collect();
            wts.sort_by(|a, b| {
                b.usage
                    .cpu_pct
                    .partial_cmp(&a.usage.cpu_pct)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            rows.extend(wts.iter().map(|w| ResourceRow {
                label: w.label.clone(),
                cpu: fmt_cpu(w.usage.cpu_pct),
                mem: fmt_mem(w.usage.rss_bytes),
                muted: false,
            }));
            rows.push(ResourceRow {
                label: "App (base)".into(),
                cpu: fmt_cpu(s.base.cpu_pct),
                mem: fmt_mem(s.base.rss_bytes),
                muted: true,
            });
        }

        let series = |label: &'static str, value: String, hist: Vec<f32>, color: gpui::Rgba| {
            div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(15.0))
                .child(div().w(px(30.0)).flex_none().text_xs().text_color(hx.tx2).child(label))
                .child(sparkline(&hist, color))
                .child(div().flex_1())
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(hx.tx0)
                        .child(value),
                )
        };

        div()
            .id("resource-band")
            .flex()
            .flex_col()
            .flex_none()
            .px_3()
            .pt_0p5()
            .pb_2()
            .gap_0p5()
            .tooltip(move |_window, cx| cx.new(|_| ResourceTooltip { rows: rows.clone() }).into())
            .child(series("CPU", cpu_label, cpu_hist, hx.accent))
            .child(series("MEM", mem_label, mem_hist, hx.green))
    }

    /// While STATS is collapsed, its totals ride on the header line
    /// ("34% · 618 MB") so collapsing it hides the sparklines, not the numbers.
    /// `None` while open (the band below shows them) or before the first sample.
    fn stats_summary(&self, hx: &HarnessTheme) -> Option<AnyElement> {
        use crate::resource_monitor::{fmt_cpu, fmt_mem};
        if !self.section_collapsed(SidebarSection::Stats) {
            return None;
        }
        let total = self.resource.as_ref()?.total;
        Some(
            div()
                .flex_none()
                .text_xs()
                .text_color(hx.tx1)
                .child(format!("{} · {}", fmt_cpu(total.cpu_pct), fmt_mem(total.rss_bytes)))
                .into_any_element(),
        )
    }

    /// PROJECTS' trailing `+` — adds a git repository.
    fn projects_add_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        self.section_add_button(
            hx,
            "project-plus",
            "Add a git repository as a project",
            |ws, window, cx| ws.add_project(window, cx),
            cx,
        )
        .into_any_element()
    }

    /// TASKS' trailing `+` — opens the inline new-task editor, expanding the
    /// section first so the editor it reveals is actually on screen.
    fn tasks_add_button(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        self.section_add_button(hx, "task-plus", "New task", |ws, window, cx| {
            if ws.prefs.collapsed_sections.remove(SidebarSection::Tasks.label()) {
                ws.save_prefs(cx);
            }
            ws.open_new_task(window, cx);
        }, cx)
        .into_any_element()
    }

    pub(crate) fn render_sidebar(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.sidebar_width))
            .h_full()
            .bg(hx.bg1)
            .border_r_1()
            .border_color(hx.border)
            .overflow_hidden()
            .child(self.section_header(hx, SidebarSection::Stats, self.stats_summary(hx), cx));
        if !self.section_collapsed(SidebarSection::Stats) {
            column = column.child(self.render_resource_band(hx, cx));
        }

        if self.data.repos.is_empty() {
            let action = self.projects_add_button(hx, cx);
            column = column.child(self.section_header(hx, SidebarSection::Projects, Some(action), cx));
            if !self.section_collapsed(SidebarSection::Projects) {
                column = column.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .mt(px(40.0))
                        .px_3()
                        .child(div().text_sm().text_color(hx.tx1).child("No projects yet"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(hx.tx2)
                                .child("Open a git repository to begin."),
                        ),
                );
            }
            return column
                .child(div().flex_1())
                .when(!self.archived_repos.is_empty(), |d| d.child(self.render_archived(hx, cx)));
        }

        // Every section header stays put; each body scrolls inside its own
        // section. TASKS takes what it needs up to a share of the sidebar, so
        // a long task list can't push PROJECTS out of view; PROJECTS takes
        // the rest, and being `flex_1` even when short keeps ARCHIVED at the
        // bottom.
        let tasks_action = self.tasks_add_button(hx, cx);
        let mut tasks_col = div()
            .flex()
            .flex_col()
            .flex_none()
            .max_h(gpui::relative(TASKS_MAX_SHARE))
            .child(self.section_header(hx, SidebarSection::Tasks, Some(tasks_action), cx));
        if !self.section_collapsed(SidebarSection::Tasks) {
            // Body gets its own top padding now that the header owns the rule.
            let mut body = div()
                .id("tasks-scroll")
                .flex()
                .flex_col()
                .flex_initial()
                .min_h_0()
                .overflow_y_scroll()
                .pt(px(4.0));
            // Rows keep their height (`flex_none`): as flex items they would
            // otherwise squash to fit before the body ever scrolled.
            for task in &self.data.tasks {
                body = body.child(div().flex_none().child(self.render_task_row(hx, task, cx)));
            }
            // An open TASKS with nothing in it read as a stray header; one
            // muted line says what the section is for and how to fill it.
            if self.data.tasks.is_empty() {
                body = body.child(
                    div()
                        .flex_none()
                        .px_3()
                        .pb_2()
                        .text_xs()
                        .text_color(hx.tx2)
                        .child("No tasks — + to start one"),
                );
            }
            tasks_col = tasks_col.child(body);
        }

        let projects_action = self.projects_add_button(hx, cx);
        let mut projects = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.section_header(hx, SidebarSection::Projects, Some(projects_action), cx));
        if !self.section_collapsed(SidebarSection::Projects) {
            let mut body = div()
                .id("projects-scroll")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .pt(px(4.0));
            for repo in &self.data.repos {
                body = body.child(div().flex_none().child(self.render_repo_group(hx, repo, cx)));
            }
            projects = projects.child(body);
        }

        column
            .child(tasks_col)
            .child(projects)
            .when(!self.archived_repos.is_empty(), |d| d.child(self.render_archived(hx, cx)))
    }

    fn render_task_row(
        &self,
        hx: &HarnessTheme,
        task: &crate::data::TaskView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let task_id = task.id.clone();
        let selected = matches!(
            &self.selection,
            Selection::Task { task_id: tid, .. } if tid == &task.id
        );
        let renaming = matches!(self.edit_kind(), Some(EditKind::Rename(rid)) if rid == &task.id);
        let task_ending = self.ending.get(&task.id).copied();
        let color = SWATCHES[(task.color as usize) % SWATCHES.len()];
        let tid_click = task_id.clone();
        let menu_id = task_id.clone();
        // A multi-project task's tile is its coordinator agent; the repo rows
        // under it are the specialists it delegates to.
        let multi = task.members.len() >= 2;
        let coord_focused = multi && self.focused_coordinator_task().as_deref() == Some(task.id.as_str());
        let coord_missing = multi && self.agent_missing(&task.id, None);
        let coord_fresh = self.restart_is_fresh(&task.id, None);
        let tid_restart = task.id.clone();
        let coord_state = if multi {
            self.task_store
                .get(&task.id)
                .map(|t| worktrees::task_coordinator_dir(&self.data_dir.join("tasks"), &t.slug))
                .and_then(|dir| self.agent_status.get(&dir).copied())
                .unwrap_or(AgentState::Unknown)
        } else {
            AgentState::Unknown
        };
        let a2a_rows = if multi { self.a2a_rows_for(&task.id) } else { Vec::new() };
        let in_flight = |tenant: Option<&str>| {
            a2a_rows
                .iter()
                .filter(|(_, t, s)| {
                    tenant.is_none_or(|x| x == t) && links::is_in_flight_delegation(t, s)
                })
                .count()
        };
        let a2a_active = in_flight(None);
        let now = self.anim_secs();

        let name_el: AnyElement = if let Some(editor) = renaming.then(|| self.edit_editor()).flatten() {
            self.render_input(hx, editor, cx).into_any_element()
        } else {
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .text_color(hx.tx0)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(task.name.clone())
                .into_any_element()
        };

        let mut block = div()
            // Unique per task: `("task-row", task.id.len())` gave every task
            // whose id happened to be the same length one shared element
            // identity, and hover state is tracked per identity.
            .id(gpui::SharedString::from(format!("task-block-{}", task.id)))
            .flex()
            .flex_col()
            .mx_1()
            .mb_1()
            .rounded_md()
            // Rows paint their own hover background; without clipping they
            // square off the corners this block just rounded.
            .overflow_hidden()
            .when(selected, |d| d.bg(hx.surface))
            .when(task_ending.is_some(), |d| d.opacity(0.55))
            // A task and its repos are one card, so hovering anywhere lifts the
            // whole block one step on the surface ramp. The row under the cursor
            // then takes a second step, which keeps it clear what a click hits —
            // the repo rows are individually selectable.
            .hover(|d| d.bg(hx.surface))
            // Rename/Archive/Delete all act on the task, so the menu opens from
            // anywhere in the block, including the repo rows — which previously
            // had no right-click of their own and so looked inert.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let pos = ev.position;
                    ws.menu = Some(CardMenu {
                        id: menu_id.clone(),
                        x: pos.x,
                        y: pos.y,
                    });
                    cx.notify();
                }),
            )
            .child(
                div()
                    .id(gpui::SharedString::from(format!("task-row-{}", task.id)))
                    .flex()
                    // Two lines now (name over branch), so the avatar and the
                    // dirty count sit with the name rather than centring
                    // themselves against the pair.
                    .items_start()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .when(coord_focused, |d| d.bg(selected_row_bg(hx)))
                    .hover({
                        let hover_bg = if multi { row_hover_bg(hx, coord_focused) } else { hx.surface_hover };
                        move |d| d.bg(hover_bg)
                    })
                    .when(multi, |d| {
                        d.tooltip(tip("Coordinator: plans the task and delegates to each repo's agent"))
                    })
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.select(
                            Selection::Task {
                                task_id: tid_click.clone(),
                                member_id: None,
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .size(px(22.0))
                            // Without flex_none the 22px box is a shrinkable
                            // flex item: a long task name squeezed it narrower
                            // than its height, so the initials sat left of
                            // centre in a box that was no longer square.
                            .flex_none()
                            .rounded_md()
                            .bg(rgb(color))
                            .flex()
                            .items_center()
                            .justify_center()
                            // Collapse the line box onto the glyphs. A default
                            // line height pads asymmetrically around the
                            // baseline (ascent > cap height, descent unused by
                            // all-caps initials), which reads as the text
                            // riding high inside the swatch.
                            .line_height(px(22.0))
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xffffff))
                            .child(task.initials.clone()),
                    )
                    // Name over branch in one column, so the branch starts at
                    // the name's first letter without hand-tuned padding that
                    // would drift if the avatar ever resized.
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(name_el)
                            .when_some(task_branch(task), |d, branch| {
                                d.child(
                                    div()
                                        .text_size(px(10.0))
                                        .font_family(hx.font_mono)
                                        .text_color(hx.tx2)
                                        .whitespace_nowrap()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(branch.to_string()),
                                )
                            }),
                    )
                    // Trailing items in one strip the avatar's height, so they
                    // centre on the name line (the row is top-aligned for the
                    // two-line name/branch). Same 10px mono and the same order
                    // as a repo row — counts, then the pill — so `~N` here
                    // lines up over the repos' `~N` below.
                    .child(
                        div()
                            .flex_none()
                            .h(px(22.0))
                            .flex()
                            .items_center()
                            .gap_1()
                            .text_size(px(10.0))
                            .font_family(hx.font_mono)
                            .when(multi && task_ending.is_none() && a2a_active > 0, |d| {
                                d.child(
                                    div()
                                        .px_1()
                                        .rounded_sm()
                                        .border_1()
                                        .border_color(hx.amber)
                                        .text_color(hx.amber)
                                        .child(format!("{a2a_active} A2A")),
                                )
                            })
                            .when(task.dirty > 0, |d| {
                                d.child(
                                    div().text_color(hx.amber).child(format!("~{}", task.dirty)),
                                )
                            })
                            .when(multi && task_ending.is_none(), |d| {
                                if coord_missing {
                                    d.child(restart_pill(hx, coord_fresh, tid_restart.clone(), None, cx))
                                } else {
                                    d.child(agent_pill(hx, coord_state, now))
                                }
                            }),
                    ),
            );
        // Multi-project tasks draw live connectors in the gutter (see
        // `links`); a single-member task keeps the plain rule.
        let mut members = if multi {
            div().relative().flex().flex_col()
        } else {
            div()
                .flex()
                .flex_col()
                .ml(px(12.0))
                .pl_1()
                .border_l_1()
                .border_color(hx.border)
        };
        let mut visuals: Vec<links::LinkVisual> = Vec::new();
        for m in task.members.iter() {
            let tid = task.id.clone();
            let mid = m.worktree_id.clone();
            // With a coordinator, the task row is the coordinator and a repo
            // row lights only when it is the one picked. A single-member task
            // has no coordinator: its row is the task, so match where the
            // selection lands (`member_id: None` resolves to it).
            let mem_selected = if multi {
                matches!(
                    &self.selection,
                    Selection::Task { task_id: t, member_id: Some(id) }
                        if t == &task.id && id == &m.worktree_id
                )
            } else {
                matches!(
                    &self.selection,
                    Selection::Task { task_id: t, .. } if t == &task.id
                ) && self.resolve().is_some_and(|r| r.path == m.path)
            };
            if multi {
                visuals.push(links::link_visual(
                    in_flight(Some(&m.worktree_id)) > 0,
                    &self.a2a_links.events_for(&task.id, &m.worktree_id),
                    now,
                ));
            }
            let label = m.repo.clone();
            let agent = self
                .agent_status
                .get(&m.path)
                .copied()
                .unwrap_or(AgentState::Unknown);
            // flex_none throughout the row's trailing items: the status pill was
            // being pushed past the sidebar's right edge because every item here
            // was shrinkable in principle and none of them could actually shrink
            // (text sets its own min width), so the row overflowed instead.
            let mut badges = div().flex().flex_none().items_center().gap_1();
            if m.ahead > 0 {
                badges = badges.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(hx.green)
                        .child(format!("↑{}", m.ahead)),
                );
            }
            if m.behind > 0 {
                badges = badges.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(hx.amber)
                        .child(format!("↓{}", m.behind)),
                );
            }
            if m.dirty > 0 {
                badges = badges.child(
                    div()
                        .text_size(px(10.0))
                        .font_family(hx.font_mono)
                        .text_color(hx.amber)
                        .child(format!("~{}", m.dirty)),
                );
            }
            members = members.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "task-mem-{}-{}",
                        task.id, m.worktree_id
                    )))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    // Fixed height and offset with connectors, so their
                    // geometry is known without measuring the layout.
                    .when(multi, |d| d.h(ui_px(links::ROW_H)).ml(ui_px(ROW_INSET)))
                    .rounded_md()
                    // The whole block is already `surface` while its task is
                    // selected, so a `surface` fill here was invisible. The
                    // accent wash every selected sidebar row uses shows on it.
                    .when(mem_selected, |d| d.bg(selected_row_bg(hx)))
                    .cursor_pointer()
                    // The second step of the hover: the block is already lifted
                    // to `surface`, so this marks which row a click would take.
                    .hover({
                        let hover_bg = row_hover_bg(hx, mem_selected);
                        move |d| d.bg(hover_bg)
                    })
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.select(
                            Selection::Task {
                                task_id: tid.clone(),
                                member_id: Some(mid.clone()),
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(hx.tx1)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(label),
                    )
                    // Only a repo that has drifted off the task's branch shows
                    // one: the branch is a task-level fact, printed once under
                    // the task name, and repeating it on every row is what left
                    // no room for the pill.
                    .when_some(member_branch(task, m), |d, branch| {
                        d.child(
                            div()
                                .min_w_0()
                                .text_size(px(10.0))
                                .font_family(hx.font_mono)
                                .text_color(hx.tx2)
                                .whitespace_nowrap()
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(branch.to_string()),
                        )
                    })
                    .child(badges)
                    .child(match task_ending {
                        Some(kind) => busy_pill(hx, kind.doing_label(), now).into_any_element(),
                        None if self.agent_missing(&task.id, Some(&m.worktree_id)) => restart_pill(
                            hx,
                            self.restart_is_fresh(&task.id, Some(&m.worktree_id)),
                            task.id.clone(),
                            Some(m.worktree_id.clone()),
                            cx,
                        )
                        .into_any_element(),
                        None => agent_pill(hx, agent, now).into_any_element(),
                    }),
            );
        }
        if multi {
            members = members.children(link_connectors(hx, &visuals));
        }
        block = block.child(members);
        block
    }

    /// The chrome around an inline edit's `Editor`. The wrapper carries the
    /// `HarnessInput` key context (so escape→`menu::Cancel` outranks the
    /// editor's clear-selection escape) and the confirm/cancel action handlers;
    /// the editor itself owns caret, selection, and clipboard.
    pub(crate) fn render_input(
        &self,
        hx: &HarnessTheme,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .key_context("HarnessInput")
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(cx.listener(Self::cancel_edit))
            .flex()
            .items_center()
            .flex_1()
            .min_w_0()
            .h(px(22.0))
            .px_1p5()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.accent)
            .text_sm()
            .text_color(hx.tx0)
            .child(editor)
    }

    pub(crate) fn render_context_menu(&self, hx: &HarnessTheme, menu: &CardMenu, cx: &mut Context<Self>) -> impl IntoElement {
        // A multi-project task opened from one of its PROJECTS rows still
        // archives/deletes as a whole; say so rather than imply one worktree.
        let whole_task = self
            .task_store
            .get(&menu.id)
            .is_some_and(|t| t.members.len() > 1);
        let (archive_label, delete_label) = if whole_task {
            ("Archive task", "Delete task…")
        } else {
            ("Archive", "Delete…")
        };
        let rename_id = menu.id.clone();
        let archive_id = menu.id.clone();
        let del_id = menu.id.clone();

        div()
            .absolute()
            .left(menu.x)
            .top(menu.y)
            .w(px(170.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            // Swallow mouse-DOWN inside the menu. The dismiss backdrop listens
            // on mouse-down, and a click is down-then-up: without this the
            // backdrop closed the menu on the way down and the item's on_click
            // never fired on the way up. That is why ＋ → Editor did nothing.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(self.menu_item(hx, "Rename", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                let name = ws
                    .task_store
                    .get(&rename_id)
                    .map(|t| t.name.clone())
                    .or_else(|| ws.manager.registry.get(&rename_id).map(|e| e.name.clone()))
                    .unwrap_or_default();
                if ws.task_store.get(&rename_id).is_some() {
                    ws.select(
                        Selection::Task {
                            task_id: rename_id.clone(),
                            member_id: None,
                        },
                        window,
                        cx,
                    );
                } else {
                    ws.select(Selection::Worktree(rename_id.clone()), window, cx);
                }
                ws.begin_edit(EditKind::Rename(rename_id.clone()), &name, "name…", window, cx);
            })))
            .child(self.menu_item(hx, archive_label, false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.request_end(archive_id.clone(), crate::workspace::lifecycle::EndKind::Archive, window, cx);
            })))
            .child(self.menu_item(hx, delete_label, true, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.request_end(del_id.clone(), crate::workspace::lifecycle::EndKind::Delete, window, cx);
            })))
    }

    pub(crate) fn menu_item(
        &self,
        hx: &HarnessTheme,
        label: &str,
        danger: bool,
        handler: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            // Key on the label itself, not its length — "Rename" and "Delete"
            // are both 6 chars, and GPUI needs unique sibling ElementIds for
            // click hit-testing. With a collision, Delete's on_click never fires.
            .id(gpui::SharedString::from(format!("menu-item-{label}")))
            .px_2()
            .py_1()
            .rounded_md()
            .text_sm()
            .text_color(if danger { hx.red } else { hx.tx1 })
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface_hover).text_color(if danger { hx.red } else { hx.tx0 }))
            .on_click(handler)
            .child(label.to_string())
    }
}

const ID_TOGGLE: &str = "repo-toggle";
const ID_MORE: &str = "repo-more";
const ID_MAIN: &str = "main-entry";
const ID_NEW_WT: &str = "repo-new-worktree";

/// Every prefix a project row keys an element id under, named rather than
/// spelled inline at the call sites so the uniqueness test below covers the
/// same set `render_repo_group` and `render_main_entry` actually render.
#[cfg(test)]
const REPO_ID_PREFIXES: [&str; 4] = [ID_TOGGLE, ID_MORE, ID_MAIN, ID_NEW_WT];

/// How far a connected repo row sits in from the task block's edge: past
/// the spine, where the old rule plus its padding put it.
const ROW_INSET: f32 = links::SPINE_X + 5.0;

/// A length given in px at the default 16px rem, scaled with the UI font
/// size (Settings ⌘± changes the rem), so the fixed-height rows and the
/// connectors drawn against them grow with the text they hold.
/// STOPPED pill for a task agent that should be running but is not; a click
/// restarts it (continuing its conversation first, fresh if that already
/// failed). Stops propagation so the row's own click does not also fire.
fn restart_pill(
    hx: &HarnessTheme,
    fresh: bool,
    task_id: String,
    member_id: Option<String>,
    cx: &mut Context<HarnessWorkspace>,
) -> gpui::Stateful<gpui::Div> {
    let tip_text = if fresh {
        "Agent not running and could not continue — click to start it fresh"
    } else {
        "Agent not running — click to restart it where it left off"
    };
    let hover_bg = tint(hx.accent, 0.16);
    let hover_fg = hx.accent;
    crate::workspace::render::stopped_pill(hx)
        .id(gpui::SharedString::from(format!(
            "restart-{task_id}-{}",
            member_id.as_deref().unwrap_or("coordinator")
        )))
        .cursor_pointer()
        .hover(move |d| d.bg(hover_bg).text_color(hover_fg))
        .tooltip(tip(tip_text))
        .on_click(cx.listener(move |ws, _, window, cx| {
            cx.stop_propagation();
            ws.restart_task_agent(task_id.clone(), member_id.clone(), window, cx);
        }))
}

fn ui_px(v: f32) -> gpui::Rems {
    gpui::rems(v / 16.0)
}

/// The coordinator → repo connectors for one task block, as absolutely
/// positioned children of its (relative) members column: a grey spine and an
/// elbow per row, then a coloured overlay along the path to every row whose
/// link is doing something, and the travelling dot.
fn link_connectors(hx: &HarnessTheme, visuals: &[links::LinkVisual]) -> Vec<AnyElement> {
    use links::{ELBOW_END_X, LinkEventKind, LinkVisual, ROW_H, SPINE_X};
    let mid = |i: usize| i as f32 * ROW_H + ROW_H / 2.0;
    let spine = |to: f32, color: gpui::Rgba| {
        div().absolute().left(ui_px(SPINE_X)).top(px(0.0)).w(px(1.0)).h(ui_px(to)).bg(color).into_any_element()
    };
    let elbow = |i: usize, color: gpui::Rgba| {
        div()
            .absolute()
            .left(ui_px(SPINE_X))
            .top(ui_px(mid(i)))
            .w(ui_px(ELBOW_END_X - SPINE_X))
            .h(px(1.0))
            .bg(color)
            .into_any_element()
    };
    let Some(last) = visuals.len().checked_sub(1) else {
        return Vec::new();
    };
    let mut out = vec![spine(mid(last) + 1.0, hx.border)];
    out.extend((0..visuals.len()).map(|i| elbow(i, hx.border)));
    let up_color = |ok: bool| if ok { hx.green } else { hx.red };
    for (i, v) in visuals.iter().enumerate() {
        let (color, alpha) = match *v {
            LinkVisual::Idle => continue,
            LinkVisual::Active { pulse } => (hx.amber, 0.35 + 0.5 * pulse),
            LinkVisual::Travel { kind: LinkEventKind::Down, .. } => (hx.amber, 0.7),
            LinkVisual::Travel { kind: LinkEventKind::Up { ok }, .. } => (up_color(ok), 0.7),
            LinkVisual::Flash { ok, fade } => (up_color(ok), fade.clamp(0.0, 1.0)),
        };
        let c = tint(color, alpha);
        out.push(spine(mid(i) + 1.0, c));
        out.push(elbow(i, c));
        if let LinkVisual::Travel { kind, progress } = *v {
            let (x, y) = links::dot_position(i, progress, kind);
            let dot = match kind {
                LinkEventKind::Down => hx.amber,
                LinkEventKind::Up { ok } => up_color(ok),
            };
            out.push(
                div()
                    .absolute()
                    .left(ui_px(x + 0.5 - 2.5))
                    .top(ui_px(y + 0.5 - 2.5))
                    .size(ui_px(5.0))
                    .rounded_full()
                    .bg(dot)
                    .into_any_element(),
            );
        }
    }
    out
}

/// The element id for one control in a project's row, keyed on the project
/// name — never on its length.
///
/// GPUI tracks hover, press and click state per element id, so
/// `("repo-toggle", repo.name.len())` gave two projects whose names were the
/// same length one shared identity. Sharing an identity shares one
/// `pending_mouse_down`, and on mouse-up the *first* of the pair painted clears
/// it (its own hitbox isn't under the cursor) before the row you actually
/// clicked can fire — so the later project was simply dead. `task-block-`
/// documents the same trap for tasks, which is where it was fixed once already.
fn repo_element_id(prefix: &str, repo_name: &str) -> gpui::SharedString {
    gpui::SharedString::from(format!("{prefix}-{repo_name}"))
}

/// The branch shown under a task's name.
///
/// A task's members are all created on one `feat/<slug>` branch, so the branch
/// is a property of the task rather than of each repo in it. Printing it once,
/// under the name, says that — and it is what leaves the repo rows narrow
/// enough for their status pill to stay inside the sidebar. The home member is
/// the reference because it is the task's own checkout.
fn task_branch(task: &crate::data::TaskView) -> Option<&str> {
    task.members
        .iter()
        .find(|m| m.is_home)
        .or_else(|| task.members.first())
        .map(|m| m.branch.as_str())
}

/// The branch to show on one repo row: `None` when it is the task's own.
///
/// A member can be moved to some other branch after the task is created, and
/// that divergence is worth seeing; agreeing with the task is not.
fn member_branch<'a>(
    task: &'a crate::data::TaskView,
    member: &'a crate::data::TaskMemberView,
) -> Option<&'a str> {
    let branch = member.branch.as_str();
    (task_branch(task) != Some(branch)).then_some(branch)
}

/// A bar sparkline for the resource band. Each sample is a thin vertical bar,
/// its height the value relative to the visible window's own peak — so the
/// shape reads even as the absolute scale (a few % vs. a whole core) shifts.
/// A floor of 1px keeps a baseline visible when everything is near zero.
fn sparkline(values: &[f32], color: gpui::Rgba) -> impl IntoElement {
    const H: f32 = 13.0;
    const BAR_W: f32 = 2.0;
    const MAX_BARS: usize = 48;

    let start = values.len().saturating_sub(MAX_BARS);
    let visible = &values[start..];
    let peak = visible.iter().copied().fold(0.0_f32, f32::max).max(f32::EPSILON);

    let mut row = div().flex().flex_row().items_end().flex_none().h(px(H)).gap(px(1.0));
    for &v in visible {
        let ratio = (v / peak).clamp(0.0, 1.0);
        let bar_h = (ratio * H).max(1.0);
        row = row.child(div().w(px(BAR_W)).h(px(bar_h)).rounded_sm().bg(color));
    }
    row
}

/// One row of the resource band's hover breakdown.
#[derive(Clone)]
struct ResourceRow {
    label: String,
    cpu: String,
    mem: String,
    /// The shared "App (base)" row is dimmed — it isn't a worktree.
    muted: bool,
}

/// The hover popover listing per-worktree usage plus the base-app row.
struct ResourceTooltip {
    rows: Vec<ResourceRow>,
}

impl Render for ResourceTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hx = theme(cx);
        let mut col = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .min_w(px(190.0))
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .rounded_md()
            .shadow_lg();

        if self.rows.is_empty() {
            return col.child(
                div().text_xs().text_color(hx.tx2).child("Measuring usage…"),
            );
        }

        for r in &self.rows {
            col = col.child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(if r.muted { hx.tx2 } else { hx.tx0 })
                            .child(r.label.clone()),
                    )
                    .child(
                        div()
                            .w(px(40.0))
                            .flex_none()
                            .text_xs()
                            .text_color(hx.tx1)
                            .child(r.cpu.clone()),
                    )
                    .child(
                        div()
                            .w(px(56.0))
                            .flex_none()
                            .text_xs()
                            .text_color(hx.tx1)
                            .child(r.mem.clone()),
                    ),
            );
        }
        col
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{TaskMemberView, TaskView};
    use std::path::PathBuf;

    fn member(repo: &str, branch: &str, is_home: bool) -> TaskMemberView {
        TaskMemberView {
            worktree_id: format!("{repo}-id"),
            repo: repo.to_string(),
            branch: branch.to_string(),
            path: PathBuf::from(format!("/wt/{repo}")),
            ahead: 0,
            behind: 0,
            dirty: 0,
            is_home,
        }
    }

    fn task(members: Vec<TaskMemberView>) -> TaskView {
        TaskView {
            id: "t1".into(),
            name: "terminal-session".into(),
            initials: "TZ".into(),
            color: 0,
            members,
            dirty: 0,
        }
    }

    #[test]
    fn the_task_branch_comes_from_the_home_member() {
        // Not simply the first: member order is registry order, and the home
        // checkout is the one the task is named for.
        let t = task(vec![
            member("sample-project", "hotfix/x", false),
            member("openorchestra", "feat/terminal-session", true),
        ]);
        assert_eq!(task_branch(&t), Some("feat/terminal-session"));
    }

    #[test]
    fn a_task_with_no_home_member_falls_back_to_the_first() {
        let t = task(vec![member("sample-project", "feat/a", false)]);
        assert_eq!(task_branch(&t), Some("feat/a"));
        assert_eq!(task_branch(&task(vec![])), None, "no members, no branch line");
    }

    #[test]
    fn a_repo_row_repeats_no_branch_it_shares_with_its_task() {
        // Repeating `feat/<slug>` on every row is what pushed the status pill
        // off the right edge of the sidebar.
        let t = task(vec![
            member("openorchestra", "feat/terminal-session", true),
            member("sample-project", "feat/terminal-session", false),
        ]);
        for m in &t.members {
            assert_eq!(member_branch(&t, m), None, "{} should stay bare", m.repo);
        }
    }

    #[test]
    fn two_projects_whose_names_are_the_same_length_get_different_element_ids() {
        // GPUI tracks hover, press and click state per element id, and the
        // *first* element painted under a shared id swallows the later one's
        // mouse-up. Seeding on `repo.name.len()` therefore left every project
        // after the first of a given name length unclickable.
        let names = ["alpha", "gamma", "openorchestra"]; // alpha/gamma collide
        let mut ids = Vec::new();
        for name in names {
            for prefix in REPO_ID_PREFIXES {
                ids.push(repo_element_id(prefix, name));
            }
        }
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len(), "element ids collide: {ids:?}");
    }

    #[test]
    fn a_repo_that_has_drifted_off_the_task_branch_shows_its_own() {
        let t = task(vec![
            member("openorchestra", "feat/terminal-session", true),
            member("sample-project", "hotfix/urgent", false),
        ]);
        assert_eq!(member_branch(&t, &t.members[0]), None);
        assert_eq!(member_branch(&t, &t.members[1]), Some("hotfix/urgent"));
    }
}
