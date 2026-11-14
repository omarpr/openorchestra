//! The PROJECTS section: each repo's group, its main checkout and worktree
//! rows, and the ARCHIVED list.

use super::*;

/// What a project-menu item does when picked.
type MenuAction = dyn Fn(&mut HarnessWorkspace, &mut Window, &mut Context<HarnessWorkspace>);

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
        // controls.
        let header_icon = |id_prefix: &'static str, glyph: AnyElement, tip_text: &'static str| {
            div()
                .id(repo_element_id(id_prefix, &repo.name))
                .group(repo_element_id(id_prefix, &repo.name).to_string())
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
        let selected = self.resolve().is_some_and(|r| r.path == wt.path);
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
        let mut badges = div().flex().flex_none().items_center().gap_1();
        if let Some(m) = member {
            for (n, glyph, color) in [(m.ahead, "↑", hx.green), (m.behind, "↓", hx.amber), (m.dirty, "~", hx.amber)] {
                if n > 0 {
                    badges = badges.child(
                        div().text_size(px(10.0)).font_family(hx.font_mono).text_color(color).child(format!("{glyph}{n}")),
                    );
                }
            }
        }
        div()
            .id(gpui::SharedString::from(format!("project-wt-{}", wt.id)))
            .flex()
            .items_center()
            .gap_2()
            .mt_1()
            .px_2()
            .py_1()
            .rounded_md()
            // Same accent left-border the selected main entry gets; the bare
            // surface fill alone barely shows against the sidebar.
            .when(selected, |d| d.bg(hx.surface).border_l_1().border_color(hx.accent))
            // Going away: dimmed and inert until it's gone.
            .when(ending.is_some(), |d| d.opacity(0.55))
            .when(ending.is_none(), |d| {
                d.cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
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
            .child(
                div()
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(hx.tx1)
                    .child("⑂"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(name_el)
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx2)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(wt.branch.clone()),
                    ),
            )
            .child(badges)
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
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx0)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child("main"),
                    )
                    // The main checkout usually sits on the default branch, but
                    // it can be on a feature branch — show where HEAD actually
                    // is. Only the default branch earns the "default ·" tag; any
                    // other branch is shown by name so it never reads as master.
                    //
                    // Clipped like the task rows: `min_w_0` above lets the column
                    // shrink, but without this the text paints straight over the
                    // status pill instead of ellipsing.
                    .child(div().text_xs().text_color(hx.tx2).whitespace_nowrap().overflow_hidden().text_ellipsis().child(if repo.head_branch == repo.default_branch {
                        format!("default · {}", repo.default_branch)
                    } else {
                        repo.head_branch.clone()
                    })),
            )
            .child(agent_pill(hx, agent, self.anim_secs()));

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
            .id(repo_element_id(ID_MAIN, &repo.name))
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
            .tooltip(tip(format!("{} · main checkout on {}", repo.name, repo.head_branch)))
            .on_click(cx.listener(move |ws, _, window, cx| {
                ws.select(Selection::Main(repo_name.clone()), window, cx);
            }))
            .child(header)
            .child(badges)
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
            .child(div().h(px(1.0)).mx_1().bg(hx.border))
            .child(item("Archive project", false, Box::new(move |ws, w, cx| ws.archive_repo(r3.clone(), w, cx))))
            .child(item(
                "Remove project…",
                true,
                Box::new(move |ws, _, cx| ws.request_removal(Removal::Project(r4.clone()), cx)),
            ))
    }
}
