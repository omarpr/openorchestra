//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    /// PROJECTS section title with a trailing `+` that adds a git repository —
    /// same header affordance as TASKS.
    fn projects_header(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(34.0))
            .px_3()
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx2)
                    .child("PROJECTS"),
            )
            .child(
                div()
                    .id("project-plus")
                    .size(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(hx.tx1)
                    .cursor_pointer()
                    .tooltip(tip("Add a git repository as a project"))
                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                    .child("+")
                    .on_click(cx.listener(|ws, _, window, cx| ws.add_project(window, cx))),
            )
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
            .py_1p5()
            .gap_0p5()
            .border_b_1()
            .border_color(hx.border)
            .tooltip(move |_window, cx| cx.new(|_| ResourceTooltip { rows: rows.clone() }).into())
            .child(series("CPU", cpu_label, cpu_hist, hx.accent))
            .child(series("MEM", mem_label, mem_hist, hx.green))
    }

    /// A sidebar section label — the small bordered strip that heads STATS
    /// (and any other label-only region). PROJECTS uses [`Self::projects_header`]
    /// so it can carry an add action.
    fn section_header(&self, hx: &HarnessTheme, label: &'static str) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(34.0))
            .px_3()
            .border_b_1()
            .border_color(hx.border)
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx2)
                    .child(label),
            )
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
            .child(self.section_header(hx, "STATS"))
            .child(self.render_resource_band(hx, cx));

        if self.data.repos.is_empty() {
            column = column
                .child(self.projects_header(hx, cx))
                .child(
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
            return column
                .child(div().flex_1())
                .when(!self.archived_repos.is_empty(), |d| d.child(self.render_archived(hx, cx)));
        }

        let mut tasks_col = div().flex().flex_col().flex_none();
        tasks_col = tasks_col.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .px_3()
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(hx.tx2)
                        .child("TASKS"),
                )
                .child(
                    div()
                        .id("task-plus")
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_md()
                        .text_color(hx.tx1)
                        .cursor_pointer()
                        .tooltip(tip("New task"))
                        .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                        .child("+")
                        .on_click(cx.listener(|ws, _, window, cx| {
                            if ws.new_task_repos.is_empty() {
                                if let Some(r) = ws.data.repos.first() {
                                    ws.new_task_repos = vec![r.name.clone()];
                                    ws.new_task_home = r.name.clone();
                                    ws.new_target_repo = r.name.clone();
                                }
                            }
                            ws.begin_edit(EditKind::NewName, "", "task name…", window, cx);
                        })),
                ),
        );
        if matches!(self.edit_kind(), Some(EditKind::NewName))
            && let Some(editor) = self.edit_editor()
        {
            tasks_col = tasks_col
                .child(div().mx_2().my_0p5().child(self.render_input(hx, editor, cx)))
                .child(self.render_new_task_members(hx, cx))
                .child(
                    div()
                        .mx_2()
                        .mb_1()
                        .text_color(hx.tx2)
                        .text_size(px(10.0))
                        .child("name · #branch · @base · click repo chip to toggle / set ★ home"),
                );
        }
        for task in &self.data.tasks {
            tasks_col = tasks_col.child(self.render_task_row(hx, task, cx));
        }

        let mut projects = div().flex().flex_col().flex_1().min_h_0().overflow_hidden().pt_1();
        projects = projects.child(self.projects_header(hx, cx));
        for repo in &self.data.repos {
            projects = projects.child(self.render_repo_group(hx, repo, cx));
        }

        column
            .child(tasks_col)
            .child(projects)
            .when(!self.archived_repos.is_empty(), |d| d.child(self.render_archived(hx, cx)))
    }

    fn render_new_task_members(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().flex().flex_wrap().gap_1().mx_2().mb_1();
        for (i, repo) in self.data.repos.iter().enumerate() {
            let name = repo.name.clone();
            let selected = self.new_task_repos.iter().any(|n| n == &name);
            let is_home = self.new_task_home == name;
            let label = if is_home {
                format!("★ {}", repo.name)
            } else if selected {
                repo.name.clone()
            } else {
                format!("+ {}", repo.name)
            };
            row = row.child(
                div()
                    .id(("new-task-repo", i))
                    .px_1p5()
                    .py_0p5()
                    .rounded_md()
                    .text_size(px(10.0))
                    .font_family(hx.font_mono)
                    .bg(if selected { hx.surface } else { hx.bg1 })
                    .text_color(if is_home {
                        hx.accent
                    } else if selected {
                        hx.tx0
                    } else {
                        hx.tx2
                    })
                    .border_1()
                    .border_color(if selected { hx.border2 } else { hx.border })
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if let Some(pos) = ws.new_task_repos.iter().position(|n| n == &name) {
                            if ws.new_task_home == name && ws.new_task_repos.len() > 1 {
                                ws.new_task_repos.remove(pos);
                                ws.new_task_home = ws.new_task_repos[0].clone();
                            } else {
                                ws.new_task_home = name.clone();
                            }
                        } else {
                            ws.new_task_repos.push(name.clone());
                            if ws.new_task_repos.len() == 1 {
                                ws.new_task_home = name.clone();
                            }
                        }
                        ws.new_target_repo = ws.new_task_home.clone();
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        row
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
        let color = SWATCHES[(task.color as usize) % SWATCHES.len()];
        let tid_click = task_id.clone();
        let menu_id = task_id.clone();

        let name_el: AnyElement = if let Some(editor) = renaming.then(|| self.edit_editor()).flatten() {
            self.render_input(hx, editor, cx).into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .text_color(hx.tx0)
                .overflow_hidden()
                .child(task.name.clone())
                .into_any_element()
        };

        let mut block = div()
            .flex()
            .flex_col()
            .mx_1()
            .mb_1()
            .rounded_md()
            .when(selected, |d| d.bg(hx.surface))
            .child(
                div()
                    .id(("task-row", task.id.len()))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
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
                            .size(px(22.0))
                            .rounded_md()
                            .bg(rgb(color))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_xs()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(rgb(0xffffff))
                            .child(task.initials.clone()),
                    )
                    .child(name_el)
                    .when(task.dirty > 0, |d| {
                        d.child(
                            div()
                                .text_xs()
                                .font_family(hx.font_mono)
                                .text_color(hx.amber)
                                .child(format!("~{}", task.dirty)),
                        )
                    }),
            );
        let mut members = div()
            .flex()
            .flex_col()
            .ml(px(12.0))
            .pl_1()
            .border_l_1()
            .border_color(hx.border);
        for (i, m) in task.members.iter().enumerate() {
            let tid = task.id.clone();
            let mid = m.worktree_id.clone();
            let mem_selected = matches!(
                &self.selection,
                Selection::Task { task_id: t, member_id: Some(id) }
                    if t == &task.id && id == &m.worktree_id
            );
            let label = if m.is_home {
                format!("★ {}", m.repo)
            } else {
                m.repo.clone()
            };
            let agent = self
                .agent_status
                .get(&m.path)
                .copied()
                .unwrap_or(AgentState::Unknown);
            let mut badges = div().flex().items_center().gap_1();
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
                    .id(("task-mem", i + task.id.len() * 17))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .py_0p5()
                    .rounded_md()
                    .when(mem_selected, |d| d.bg(hx.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
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
                            .text_xs()
                            .text_color(if m.is_home { hx.accent } else { hx.tx1 })
                            .child(label),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .child(m.branch.clone()),
                    )
                    .child(badges)
                    .child(agent_pill(hx, agent)),
            );
        }
        block = block.child(members);
        block
    }

    pub(crate) fn render_repo_group(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.prefs.collapsed.contains(&repo.name);
        let repo_name = repo.name.clone();
        let archive_repo = repo.name.clone();
        let trash_repo = repo.name.clone();
        let trash_pending = self.pending_repo_trash.as_deref() == Some(repo.name.as_str());
        let count = repo.worktrees.len();

        // A small icon button in the repo header (archive, trash), styled like
        // the section `+` controls. `danger` tints it red — used to flag the
        // trash's armed (click-again-to-confirm) state.
        let header_icon =
            |id_seed: usize, glyph: &'static str, tip_text: &'static str, danger: bool| {
                div()
                    .id(("repo-icon", id_seed))
                    .size(px(20.0))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_xs()
                    .text_color(if danger { hx.red } else { hx.tx2 })
                    .cursor_pointer()
                    .tooltip(tip(tip_text))
                    .hover(|d| d.bg(hx.surface).text_color(if danger { hx.red } else { hx.tx0 }))
                    .child(glyph)
            };

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .child(
                div()
                    .id(("repo-toggle", repo.name.len()))
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        // Persisted, so a collapsed project stays collapsed
                        // across restarts. `remove` on expand keeps the prefs
                        // file to just the collapsed ones (see Prefs::collapsed).
                        if !ws.prefs.collapsed.remove(&repo_name) {
                            ws.prefs.collapsed.insert(repo_name.clone());
                        }
                        ws.save_prefs(cx);
                        cx.notify();
                    }))
                    .child(div().text_xs().text_color(hx.tx2).child(if collapsed { "▸" } else { "▾" }))
                    .child(div().flex_1().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(repo.name.clone()))
                    .child(div().text_xs().text_color(hx.tx2).child(format!("{count} {}", if count == 1 { "tree" } else { "trees" }))),
            )
            // Archive / hooks / trash. Hooks opens the setup script in the
            // editor (TOML is harness-managed).
            .child(
                header_icon(repo.name.len(), "▦", "Archive project", false).on_click(
                    cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.archive_repo(archive_repo.clone(), window, cx);
                    }),
                ),
            )
            .child(
                header_icon(repo.name.len() + 500, "ƒ", "Edit setup scripts", false).on_click({
                    let hooks_repo = repo.name.clone();
                    cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.open_project_hooks(
                            &hooks_repo,
                            HookScriptPhase::Setup,
                            HookLayer::Repo,
                            window,
                            cx,
                        );
                    })
                }),
            )
            .child(
                header_icon(repo.name.len() + 1000, "✕", "Remove project (files kept)", trash_pending)
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.trash_repo(trash_repo.clone(), window, cx);
                    })),
            );

        let mut group = div().flex().flex_col().child(header);

        if !collapsed {
            let mut body = div()
                .flex()
                .flex_col()
                .ml(px(15.0))
                .pl_1()
                .border_l_1()
                .border_color(hx.border);
            // Projects foot: main checkout only (tasks own the worktree cards).
            body = body.child(self.render_main_entry(hx, repo, cx));
            group = group.child(body);
        }
        group
    }

    /// The restorable "ARCHIVED" list at the foot of the sidebar. Each row
    /// restores its project on click. Rendered only when something is archived.
    pub(crate) fn render_archived(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut section = div()
            .flex()
            .flex_col()
            .flex_none()
            .border_t_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.0))
                    .px_3()
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child("ARCHIVED")),
            );

        for (ix, root) in self.archived_repos.iter().enumerate() {
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            let restore_root = root.clone();
            section = section.child(
                div()
                    .id(("archived-row", ix))
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .cursor_pointer()
                    .tooltip(tip("Restore project"))
                    .hover(|d| d.bg(hx.surface))
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.restore_repo(restore_root.clone(), window, cx);
                    }))
                    .child(div().flex_1().min_w_0().text_sm().text_color(hx.tx1).child(name))
                    .child(div().flex_none().text_xs().text_color(hx.tx2).child("restore ⤴")),
            );
        }
        section
    }

    pub(crate) fn render_main_entry(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.selection == Selection::Main(repo.name.clone());
        let repo_name = repo.name.clone();

        // Live agent state on the main checkout — main is first-class (D8
        // revised), so an agent run on it shows the same pill a worktree does,
        // keyed on the repo root the status watcher now watches.
        let agent = self
            .agent_status
            .get(&repo.repo_root)
            .copied()
            .unwrap_or(AgentState::Unknown);

        let header = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(tint(hx.accent, 0.15))
                    .text_xs()
                    .text_color(hx.accent)
                    .child("⌂"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_sm().text_color(hx.tx0).child("main"))
                    // The main checkout usually sits on the default branch, but
                    // it can be on a feature branch — show where HEAD actually
                    // is. Only the default branch earns the "default ·" tag; any
                    // other branch is shown by name so it never reads as master.
                    .child(div().text_xs().text_color(hx.tx2).child(if repo.head_branch == repo.default_branch {
                        format!("default · {}", repo.default_branch)
                    } else {
                        repo.head_branch.clone()
                    })),
            )
            .child(agent_pill(hx, agent));

        // Badges mirror the worktree card's, minus the PR badge: a PR is a
        // feature-branch concept, and main is the target, not a PR head. CI is
        // the default branch's checks.
        let fs = self.forge_status.get(&repo.default_branch);
        let mut badges = div().flex().items_center().gap_2().mt_1();
        if repo.ahead > 0 {
            badges = badges.child(div().text_xs().text_color(hx.green).child(format!("↑{}", repo.ahead)));
        }
        if repo.behind > 0 {
            badges = badges.child(div().text_xs().text_color(hx.amber).child(format!("↓{}", repo.behind)));
        }
        if repo.dirty > 0 {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(hx.amber)
                    .child(div().size(px(5.0)).rounded_full().bg(hx.amber))
                    .child(format!("~{}", repo.dirty)),
            );
        }
        if let Some((c, label)) = fs.and_then(|s| ci_summary(hx, &s.checks)) {
            badges = badges.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(c)
                    .child(div().size(px(5.0)).rounded_full().bg(c))
                    .child(label),
            );
        }

        div()
            .id(("main-entry", repo.name.len() + 1))
            .flex()
            .flex_col()
            .mx_1()
            .my_0p5()
            .p_2()
            .rounded_md()
            // Same accent left-border a selected worktree card gets, so "which
            // entry is selected" reads the same whether it's main or a worktree.
            .when(selected, |d| d.bg(hx.surface).border_l_1().border_color(hx.accent))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Main(repo_name.clone()), window, cx);
            }))
            .child(header)
            .child(badges)
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
        let confirming = self.pending_delete.as_deref() == Some(menu.id.as_str());
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
            .child(self.menu_item(hx, "Archive", false, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.archive(archive_id.clone(), window, cx);
            })))
            .child(self.menu_item(hx, if confirming { "Confirm delete?" } else { "Delete" }, true, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                if ws.pending_delete.as_deref() == Some(del_id.as_str()) {
                    ws.delete(del_id.clone(), window, cx);
                } else {
                    ws.pending_delete = Some(del_id.clone());
                    cx.notify();
                }
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
