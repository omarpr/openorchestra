//! The PROJECTS section: each repo's group, its main checkout and worktree
//! rows, and the ARCHIVED list.

use super::*;
use crate::workspace::remote::worktree_pane_key;

/// What a project-menu item does when picked.
type MenuAction = dyn Fn(&mut HarnessWorkspace, &mut Window, &mut Context<HarnessWorkspace>);

/// Width of a row in a reorder panel (panel 240 − 2 × 6 padding),
/// shared with the drag ghost so the ghost is the row's exact size.
const REORDER_ROW_W: f32 = 228.0;

/// A reorder row's inside: the name, then the four-dot grip on the right.
fn reorder_row_content(hx: &HarnessTheme, name: &str, grip: gpui::Div) -> gpui::Div {
    div()
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap_1()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .text_sm()
                .text_color(hx.tx0)
                .child(name.to_string()),
        )
        .child(div().size(px(22.0)).flex().flex_none().items_center().justify_center().child(grip))
}

/// The row that follows the cursor while dragging in a reorder panel: the
/// same row, lifted (raised surface, accent border, shadow).
struct ReorderRowGhost {
    name: String,
    hx: HarnessTheme,
}

impl Render for ReorderRowGhost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let hx = &self.hx;
        let dot = || div().size(px(3.0)).rounded_full().bg(hx.tx1);
        let grip = div()
            .flex()
            .flex_col()
            .gap(px(3.0))
            .child(div().flex().gap(px(3.0)).child(dot()).child(dot()))
            .child(div().flex().gap(px(3.0)).child(dot()).child(dot()));
        div()
            .w(px(REORDER_ROW_W))
            .flex()
            .items_center()
            .pl_2()
            .pr_1()
            .py_1()
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.accent)
            .shadow_lg()
            .cursor(gpui::CursorStyle::ClosedHand)
            .child(reorder_row_content(hx, &self.name, grip))
    }
}

impl HarnessWorkspace {
    pub(crate) fn render_repo_group(
        &self,
        hx: &HarnessTheme,
        repo: &crate::data::RepoView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let collapsed = self.prefs.collapsed.contains(&repo.name);
        let repo_name = repo.name.clone();
        let count = repo.worktrees.len();

        // A small icon button in the repo header, styled like the section `+`
        // controls. Shown only while the header is hovered (or its menu is
        // open): on every project at once they were clutter, and they are
        // second-nature targets for the row you're already pointing at.
        let header_group = repo_element_id("repo-header", &repo.name);
        let menu_open = self.project_menu.as_ref().is_some_and(|(r, _, _)| r == &repo.name);
        let header_icon = |id_prefix: &'static str, glyph: AnyElement, tip_text: &'static str| {
            div()
                .id(repo_element_id(id_prefix, &repo.name))
                .when(!menu_open, |d| d.opacity(0.0).group_hover(header_group.clone(), |s| s.opacity(1.0)))
                .size(px(20.0))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .rounded_md()
                .text_xs()
                .text_color(hx.tx2)
                .cursor_pointer()
                .tooltip(tip(tip_text))
                .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                .child(glyph)
        };
        let text = |glyph: &'static str| div().child(glyph).into_any_element();

        let header = div()
            .group(header_group.clone())
            .flex()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .child(
                div()
                    .id(repo_element_id(ID_TOGGLE, &repo.name))
                    .flex()
                    .flex_1()
                    // Shrinkable, with the name ellipsing below: four header
                    // icons no longer fit beside a long project name, and
                    // without this the row overflowed and pushed the trash
                    // button past the sidebar's edge.
                    .min_w_0()
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
                    .child(div().flex_1().min_w_0().whitespace_nowrap().overflow_hidden().text_ellipsis().text_sm().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(repo.name.clone()))
                    .when(count > 0, |d| {
                        d.child(div().flex_none().whitespace_nowrap().text_xs().text_color(hx.tx2).child(format!(
                            "{count} {}",
                            if count == 1 { "worktree" } else { "worktrees" }
                        )))
                    }),
            )
            // New worktree, then the ⋯ menu (setup scripts, archive, remove).
            //
            // `+` is the classic per-project entry point that the task-centric
            // rework (#3) dropped: it opens the same new-task dialog TASKS' `+`
            // does, scoped to this one project, so the worktree it makes gets
            // the agent, branch and setup hooks every task does — and is
            // listed both here and under TASKS.
            .child(
                header_icon(ID_NEW_WT, text("+"), "New worktree in this project").on_click({
                    let new_wt_repo = repo.name.clone();
                    cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.open_new_task_for_repo(&new_wt_repo, window, cx);
                    })
                }),
            )
            // Everything else lives in one labeled menu (also on right-click),
            // so "remove" is a word in a list rather than an × that sat one
            // icon away from archive.
            .child(
                header_icon(ID_MORE, text("⋯"), "More actions").on_mouse_down(MouseButton::Left, {
                    let menu_repo = repo.name.clone();
                    cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        ws.project_menu = Some((menu_repo.clone(), ev.position.x - px(150.0), ev.position.y + px(8.0)));
                        cx.notify();
                    })
                }),
            );
        let header = div()
            .on_mouse_down(MouseButton::Right, {
                let menu_repo = repo.name.clone();
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    ws.project_menu = Some((menu_repo.clone(), ev.position.x, ev.position.y));
                    cx.notify();
                })
            })
            .child(header);

        let mut group = div().flex().flex_col().child(header);

        if !collapsed {
            let mut body = div()
                .flex()
                .flex_col()
                .ml(px(15.0))
                .pl_1()
                .border_l_1()
                .border_color(hx.border);
            // Main checkout first, then the project's worktrees. Tasks still
            // own the full cards; these rows are the per-project view of the
            // same worktrees, so a single-repo workflow never has to leave
            // PROJECTS.
            body = body.child(self.render_main_entry(hx, repo, cx));
            for wt in &repo.worktrees {
                body = body.child(self.render_project_worktree_row(hx, wt, cx));
            }
            group = group.child(body);
        }
        group
    }

    /// One worktree under its project in PROJECTS: name, branch, badges and
    /// agent pill. Clicking it selects the owning task focused on this member,
    /// i.e. exactly what clicking the same row under TASKS does.
    pub(crate) fn render_project_worktree_row(
        &self,
        hx: &HarnessTheme,
        wt: &crate::data::WorktreeView,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sel = match &wt.task_id {
            Some(t) => Selection::Task { task_id: t.clone(), member_id: Some(wt.id.clone()) },
            None => Selection::Worktree(wt.id.clone()),
        };
        // Match on where the selection actually lands, not its exact shape: a
        // task selected from TASKS, a restore, or the conductor can carry
        // `member_id: None` (the home member) or a bare `Worktree` id, and an
        // exact `==` against this row's own selection missed those.
        // A task's worktree shows its selection under TASKS; marking it here
        // too put the indicator in the wrong section. Fall back to this row
        // only while TASKS is collapsed, so the selection never disappears.
        let shown_in_tasks =
            wt.task_id.is_some() && !self.section_collapsed(super::SidebarSection::Tasks);
        let selected = !shown_in_tasks && self.resolve().is_some_and(|r| r.path == wt.path);
        // Counts live on the task member (the live status pass keeps them
        // current); a worktree with no task has none to show.
        let member = self
            .data
            .tasks
            .iter()
            .flat_map(|t| t.members.iter())
            .find(|m| m.worktree_id == wt.id);
        let agent = self.agent_status.get(&wt.path).copied().unwrap_or(AgentState::Unknown);
        let ending = self.ending_for(wt.task_id.as_deref(), &wt.id);
        // Rename acts on the owning task (that is what the menu keys on), so
        // the field opens here as well as on the TASKS card — before, a rename
        // started from this row put its input in TASKS, collapsed or not, and
        // looked like nothing happened.
        let rename_key = wt.task_id.as_deref().unwrap_or(&wt.id);
        let renaming = matches!(self.edit_kind(), Some(EditKind::Rename(rid)) if rid == rename_key);
        let name_el: AnyElement = match renaming.then(|| self.edit_editor()).flatten() {
            // Clicks in the field stay in the field: the row's own click would
            // re-select it and pull focus back to the terminal.
            Some(editor) => div()
                .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
                .child(self.render_input(hx, editor, cx))
                .into_any_element(),
            None => div()
                .text_sm()
                .text_color(hx.tx0)
                .whitespace_nowrap()
                .overflow_hidden()
                .text_ellipsis()
                .child(wt.name.clone())
                .into_any_element(),
        };
        let counts = member.and_then(|m| git_counts(hx, m.ahead, m.behind, m.dirty));
        div()
            .id(gpui::SharedString::from(format!("project-wt-{}", wt.id)))
            .flex()
            .items_center()
            .gap_2()
            .mt_1()
            .px_2()
            .py_1()
            .rounded_md()
            .when(selected, |d| d.bg(selected_row_bg(hx)))
            // Going away: dimmed and inert until it's gone.
            .when(ending.is_some(), |d| d.opacity(0.55))
            .when(ending.is_none(), |d| {
                let hover_bg = row_hover_bg(hx, selected);
                d.cursor_pointer()
                    .hover(move |d| d.bg(hover_bg))
                    .on_click(cx.listener(move |ws, _, window, cx| ws.select(sel.clone(), window, cx)))
            })
            // The same Rename / Archive / Delete menu the TASKS card has —
            // keyed on the owning task, since that is what those act on.
            .on_mouse_down(MouseButton::Right, {
                let menu_id = wt.task_id.clone().unwrap_or_else(|| wt.id.clone());
                cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    ws.menu = Some(CardMenu { id: menu_id.clone(), x: ev.position.x, y: ev.position.y });
                    cx.notify();
                })
            })
            // Name and branch both ellipsize in a narrow sidebar; the tooltip
            // always has them in full.
            .tooltip(tip(format!("{} · {} — right-click to rename, archive or delete", wt.name, wt.branch)))
            // Grey while it's being archived or deleted: the busy pill says
            // so, and a state colour there would be about an agent going away.
            .child(state_tile(
                hx,
                ICON_WORKTREE,
                if ending.is_some() { AgentState::Unknown } else { agent },
                self.anim_secs(),
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(name_el)
                    // Branch, then its git counts on the same line: on a line
                    // of their own the counts made rows uneven heights.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1p5()
                            .child(
                                div()
                                    .min_w_0()
                                    .text_xs()
                                    .font_family(hx.font_mono)
                                    .text_color(hx.tx2)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(wt.branch.clone()),
                            )
                            .children(counts),
                    ),
            )
                        .children(self.remote_dot(
                hx,
                format!("remote-dot-wt-{}", wt.id),
                &worktree_pane_key(wt.task_id.as_deref(), &wt.id),
            ))
            .child(match ending {
                Some(kind) => busy_pill(hx, kind.doing_label(), self.anim_secs()).into_any_element(),
                None => agent_pill(hx, agent, self.anim_secs()).into_any_element(),
            })
    }

    /// The restorable "ARCHIVED" list at the foot of the sidebar. Each row
    /// restores its project on click. Rendered only when something is archived.
    /// The ARCHIVED section: collapsed by default to one header line with a
    /// count; open, newest first, at most [`ARCHIVED_VISIBLE_ROWS`] tall before
    /// it scrolls inside itself, so it never crowds out PROJECTS. Only the
    /// "restore" link restores — a stray click on the row does nothing — and
    /// "×" drops an entry from the list (files stay on disk).
    pub(crate) fn render_archived(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        let count = div()
            .flex_none()
            .text_xs()
            .font_family(hx.font_mono)
            .text_color(hx.tx2)
            .child(self.archived_repos.len().to_string())
            .into_any_element();
        let mut section = div()
            .flex()
            .flex_col()
            .flex_none()
            .child(self.section_header(hx, SidebarSection::Archived, Some(count), cx));
        if self.section_collapsed(SidebarSection::Archived) {
            return section;
        }

        let mut rows = div()
            .id("archived-scroll")
            .flex()
            .flex_col()
            .max_h(px(ARCHIVED_VISIBLE_ROWS * ARCHIVED_ROW_H))
            .overflow_y_scroll();
        // Archiving appends, so the newest is last; show it first.
        for (ix, root) in self.archived_repos.iter().enumerate().rev() {
            let name = root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            let restore_root = root.clone();
            let forget_root = root.clone();
            rows = rows.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(ARCHIVED_ROW_H))
                    .px_3()
                    .hover(|d| d.bg(hx.surface))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child(name),
                    )
                    .child(
                        div()
                            .id(("archived-restore", ix))
                            .flex_none()
                            .px_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(hx.tx2)
                            .cursor_pointer()
                            .tooltip(tip("Restore project"))
                            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
                            .child("restore ⤴")
                            .on_click(cx.listener(move |ws, _, window, cx| {
                                ws.restore_repo(restore_root.clone(), window, cx);
                            })),
                    )
                    .child(
                        div()
                            .id(("archived-forget", ix))
                            .flex_none()
                            .px_1()
                            .rounded_md()
                            .text_xs()
                            .text_color(hx.tx2)
                            .cursor_pointer()
                            .tooltip(tip("Remove from this list (files stay on disk)"))
                            .hover(|d| d.bg(hx.surface_hover).text_color(hx.red))
                            .child("×")
                            .on_click(cx.listener(move |ws, _, _, cx| {
                                ws.request_removal(Removal::Archived(forget_root.clone()), cx);
                            })),
                    ),
            );
        }
        section = section.child(rows);
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

        // The branch HEAD is on is the row's name — it is what you scan for.
        // Titled "main" with the branch underneath, a main checkout on
        // `review-fixes` read as a contradiction. Git counts and CI share the
        // subtitle line, so every row is the same two lines tall.
        let fs = self.forge_status.get(&repo.default_branch);
        let ci = fs.and_then(|s| ci_summary(hx, &s.checks)).map(|(c, label)| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1()
                .text_xs()
                .text_color(c)
                .child(div().size(px(5.0)).rounded_full().bg(c))
                .child(label)
        });
        // "<default branch> · ↑7 ~9": the branch this checkout tracks says
        // which row it is without a phrase for it, and the dot only appears
        // when there are counts to separate.
        let counts = git_counts(hx, repo.ahead, repo.behind, repo.dirty);
        let subtitle = div()
            .flex()
            .items_center()
            .gap_1p5()
            .text_xs()
            .text_color(hx.tx2)
            .child(
                div()
                    .min_w_0()
                    .font_family(hx.font_mono)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(repo.default_branch.clone()),
            )
            .when(counts.is_some(), |d| d.child(div().flex_none().child("·")))
            .children(counts)
            .children(ci);

        div()
            .id(repo_element_id(ID_MAIN, &repo.name))
            .flex()
            .items_center()
            .gap_2()
            .mx_1()
            .my_0p5()
            .p_2()
            .rounded_md()
            .when(selected, |d| d.bg(selected_row_bg(hx)))
            .cursor_pointer()
            .hover({
                let hover_bg = row_hover_bg(hx, selected);
                move |d| d.bg(hover_bg)
            })
            .tooltip(tip(format!("{} · main checkout on {}", repo.name, repo.head_branch)))
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Main(repo_name.clone()), window, cx);
            }))
            .child(state_tile(hx, ICON_MAIN_CHECKOUT, agent, self.anim_secs()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx0)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(repo.head_branch.clone()),
                    )
                    .child(subtitle),
            )
                        .children(self.remote_dot(
                hx,
                format!("remote-dot-main-{}", repo.name),
                &format!("main:{}", repo.name),
            ))
            .child(agent_pill(hx, agent, self.anim_secs()))
    }

    /// The "Reorder projects" / "Reorder tasks" panel: every row with a
    /// four-dot grip on the right; drag a whole row onto another row to move
    /// it there.
    pub(crate) fn render_reorder_panel(
        &self,
        hx: &HarnessTheme,
        list_kind: ReorderList,
        x: Pixels,
        y: Pixels,
        list_max_h: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Four dots in a 2×2 grid — the drag handle.
        let grip = |color: gpui::Rgba| {
            let dot = || div().size(px(3.0)).rounded_full().bg(color);
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(div().flex().gap(px(3.0)).child(dot()).child(dot()))
                .child(div().flex().gap(px(3.0)).child(dot()).child(dot()))
        };
        // (key used to move it, label shown)
        let rows: Vec<(String, String)> = match list_kind {
            ReorderList::Projects => self.data.repos.iter().map(|r| (r.name.clone(), r.name.clone())).collect(),
            ReorderList::Tasks => self.data.tasks.iter().map(|t| (t.id.clone(), t.name.clone())).collect(),
        };
        let title = match list_kind {
            ReorderList::Projects => "REORDER PROJECTS",
            ReorderList::Tasks => "REORDER TASKS",
        };
        let mut list = div()
            .id("reorder-list")
            .flex()
            .flex_col()
            .gap_0p5()
            .min_h_0()
            .max_h(list_max_h)
            .overflow_y_scroll();
        for (i, (key, label)) in rows.into_iter().enumerate() {
            let drop_key = key.clone();
            let ghost_label = label.clone();
            let ghost_theme = hx.clone();
            let drop_bg = tint(hx.accent, 0.16);
            list = list.child(
                div()
                    .id(("reorder-row", i))
                    .flex()
                    .items_center()
                    .gap_1()
                    .pl_2()
                    .pr_1()
                    .py_1()
                    .rounded_md()
                    // The whole row is the drag handle; the grip just says so.
                    .cursor(gpui::CursorStyle::OpenHand)
                    .hover(|d| d.bg(hx.surface))
                    .tooltip(tip("Drag to reorder"))
                    // What follows the cursor is the row itself, at its own
                    // width, so it reads as moving the row — not a label.
                    .on_drag(DraggedReorderRow { list: list_kind, key }, move |_, _, _, cx| {
                        let name = ghost_label.clone();
                        let hx = ghost_theme.clone();
                        cx.new(|_| ReorderRowGhost { name, hx })
                    })
                    // The row under the cursor while dragging is where the
                    // dragged row will land (same list only).
                    .drag_over::<DraggedReorderRow>(move |d, dragged, _, _| {
                        if dragged.list == list_kind { d.bg(drop_bg) } else { d }
                    })
                    .on_drop(cx.listener(move |ws, dragged: &DraggedReorderRow, window, cx| {
                        if dragged.list != list_kind {
                            return;
                        }
                        match list_kind {
                            ReorderList::Projects => ws.move_repo(&dragged.key, &drop_key, window, cx),
                            ReorderList::Tasks => ws.move_task(&dragged.key, &drop_key, window, cx),
                        }
                    }))
                    .child(reorder_row_content(hx, &label, grip(hx.tx2))),
            );
        }
        div()
            .absolute()
            .left(x.max(px(8.0)))
            .top(y)
            .w(px(240.0))
            .p(px(6.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
            .child(
                div()
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(hx.tx2)
                    .child(title),
            )
            .child(list)
            .child(
                div()
                    .id("reorder-done")
                    .mt_1()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .text_sm()
                    .text_center()
                    .text_color(hx.tx0)
                    .bg(hx.surface)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child("Done")
                    .on_click(cx.listener(|ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.reorder_panel = None;
                        cx.notify();
                    })),
            )
    }

    /// The project header's ⋯ / right-click menu.
    pub(crate) fn render_project_menu(
        &self,
        hx: &HarnessTheme,
        repo: &str,
        x: Pixels,
        y: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let item = |label: &'static str, danger: bool, run: Box<MenuAction>| {
            self.menu_item(hx, label, danger, cx.listener(move |ws, _, window, cx| {
                cx.stop_propagation();
                ws.project_menu = None;
                run(ws, window, cx);
            }))
        };
        let (r1, r2, r3, r4) = (repo.to_string(), repo.to_string(), repo.to_string(), repo.to_string());
        let (up, down) = (self.repo_can_shift(repo, true), self.repo_can_shift(repo, false));
        let (r5, r6) = (repo.to_string(), repo.to_string());
        div()
            .absolute()
            .left(x)
            .top(y)
            .w(px(190.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .flex()
            .flex_col()
            .gap_1()
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
            .child(item("New worktree…", false, Box::new(move |ws, w, cx| ws.open_new_task_for_repo(&r1, w, cx))))
            .child(item(
                "Edit setup scripts…",
                false,
                Box::new(move |ws, w, cx| ws.open_project_hooks(&r2, HookScriptPhase::Setup, HookLayer::Repo, w, cx)),
            ))
            .when(up, |d| d.child(item("Move up", false, Box::new(move |ws, w, cx| ws.shift_repo(&r5, true, w, cx)))))
            .when(down, |d| d.child(item("Move down", false, Box::new(move |ws, w, cx| ws.shift_repo(&r6, false, w, cx)))))
            .child(div().h(px(1.0)).mx_1().bg(hx.border))
            .child(item("Archive project", false, Box::new(move |ws, w, cx| ws.archive_repo(r3.clone(), w, cx))))
            .child(item(
                "Remove project…",
                true,
                Box::new(move |ws, _, cx| ws.request_removal(Removal::Project(r4.clone()), cx)),
            ))
    }
}
