//! Centered "New task" dialog — name, brief, optional branch, project members —
//! and its single-project "New worktree" mode, opened from a PROJECTS row.

use super::super::*;

impl HarnessWorkspace {
    /// Push `feat/{slug}` into the branch field placeholder when the name changes.
    pub(crate) fn sync_new_task_branch_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let auto = self.new_task.as_ref().map(|dialog| {
            let name = dialog.name.read(cx).text(cx);
            format!("feat/{}", slugify(name.trim()))
        });
        let Some(auto) = auto else { return };
        let Some(dialog) = self.new_task.as_mut() else { return };
        if dialog.auto_branch_shown == auto {
            return;
        }
        dialog.branch.update(cx, |e, cx| {
            e.set_placeholder_text(auto.as_str(), window, cx);
        });
        dialog.auto_branch_shown = auto;
    }

    /// Full-window overlay with the new-task form. Click outside or Escape cancels.
    pub(crate) fn render_new_task_dialog(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(dialog) = self.new_task.as_ref() else {
            return div().into_any_element();
        };

        // Worktree mode: opened from a PROJECTS row, for that project only.
        let worktree_for = dialog.worktree_for.clone();
        let is_worktree = worktree_for.is_some();
        let name_editor = dialog.name.clone();
        let brief_editor = dialog.brief.clone();
        let branch_editor = dialog.branch.clone();

        let field_label = |label: &str| {
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx2)
                .child(label.to_string())
        };

        let card = div()
            .id("new-task-card")
            .w(px(480.0))
            .max_h(px(640.0))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .shadow_lg()
            .key_context("HarnessInput")
            .on_action(cx.listener(Self::confirm_edit))
            .on_action(cx.listener(Self::cancel_edit))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(hx.tx0)
                            .child(if is_worktree { "New worktree" } else { "New task" }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(self.render_new_task_info(hx, is_worktree))
                            .child(
                                div()
                                    .id("new-task-close")
                                    .px_2()
                                    .rounded_md()
                                    .text_sm()
                                    .text_color(hx.tx2)
                                    .cursor_pointer()
                                    .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                                    .child("✕")
                                    .on_click(cx.listener(|ws, _, _, cx| {
                                        ws.close_new_task(cx);
                                    })),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label("Name"))
                    .child(self.render_input(hx, name_editor, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label(if is_worktree { "Brief (optional)" } else { "Brief" }))
                    .child(self.render_multiline_input(hx, brief_editor, cx))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(hx.tx2)
                            .child(if is_worktree {
                                "Given to the agent as its starting prompt."
                            } else {
                                "Shared with every agent as the starting prompt."
                            }),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label("Branch"))
                    .child(self.render_input(hx, branch_editor, cx))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(hx.tx2)
                            .child("Leave empty to use the placeholder name."),
                    ),
            )
            .child(match &worktree_for {
                // One project, fixed by where `+` was clicked: shown, not picked.
                Some(repo) => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label("Project"))
                    .child(
                        div()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .text_color(hx.tx1)
                            .child(repo.clone()),
                    ),
                None => div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_h_0()
                    .child(field_label("Projects"))
                    .child(
                        div()
                            .id("new-task-projects")
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .child(self.render_new_task_members(hx, cx)),
                    ),
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .mt_1()
                    .child(
                        div()
                            .id("new-task-cancel")
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
                            .hover(|d| d.border_color(hx.border).text_color(hx.tx0))
                            .child("Cancel")
                            .on_click(cx.listener(|ws, _, _, cx| ws.close_new_task(cx))),
                    )
                    .child(
                        div()
                            .id("new-task-create")
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
                            .hover(|d| d.border_color(hx.accent).text_color(hx.accent))
                            .child(if is_worktree { "Create worktree" } else { "Create" })
                            .on_click(cx.listener(|ws, _, window, cx| {
                                ws.submit_new_task(window, cx);
                            })),
                    ),
            );

        div()
            .id("new-task-backdrop")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|ws, _, _, cx| {
                ws.close_new_task(cx);
            }))
            .child(card)
            .into_any_element()
    }

    /// Info control — name vs brief vs home vs multi-project context.
    fn render_new_task_info(&self, hx: &HarnessTheme, is_worktree: bool) -> impl IntoElement {
        div()
            .id("new-task-info")
            .size(px(20.0))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(hx.tx1)
            .cursor_default()
            .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
            .tooltip(tip(if is_worktree {
                "Creates a git worktree of this project on its own branch, with an agent and the project's setup scripts. It is listed under the project and under TASKS."
            } else {
                "Name labels the effort. Brief is the starting prompt injected into agents. Home is where the orchestrator runs. Multi-project tasks coordinate via A2A — project AGENTS.md is never overwritten."
            }))
            .child("ⓘ")
    }

    fn render_new_task_members(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> impl IntoElement {
        // One row per project, previewing the post-create member list: checkbox
        // for include, Home radio among included members.
        let mut col = div()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(hx.border)
            .pl_1();
        for (i, repo) in self.data.repos.iter().enumerate() {
            let name = repo.name.clone();
            let included = self.new_task_repos.iter().any(|n| n == &name);
            let is_home = included && self.new_task_home == name;
            let check = if included { "☑" } else { "☐" };
            let label = if is_home {
                format!("★ {}", repo.name)
            } else {
                repo.name.clone()
            };
            let name_for_check = name.clone();
            let name_for_home = name.clone();

            let mut row = div()
                .id(("new-task-repo", i))
                .flex()
                .items_center()
                .gap_1()
                .px_1()
                .py_0p5()
                .rounded_md()
                .hover(|d| d.bg(hx.surface_hover));

            row = row.child(
                div()
                    .id(("new-task-check", i))
                    .flex_none()
                    .size(px(16.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.0))
                    .text_color(if included { hx.tx0 } else { hx.tx2 })
                    .cursor_pointer()
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        cx.stop_propagation();
                        ws.toggle_new_task_repo(&name_for_check, cx);
                    }))
                    .child(check),
            );

            row = row.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(if is_home {
                        hx.accent
                    } else if included {
                        hx.tx1
                    } else {
                        hx.tx2
                    })
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .child(label),
            );

            if included {
                let radio = if is_home { "●" } else { "○" };
                row = row.child(
                    div()
                        .id(("new-task-home", i))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_0p5()
                        .cursor_pointer()
                        .text_size(px(10.0))
                        .text_color(if is_home { hx.accent } else { hx.tx2 })
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            ws.set_new_task_home(&name_for_home, cx);
                        }))
                        .child(radio)
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(if is_home { hx.accent } else { hx.tx2 })
                                .child("Home"),
                        ),
                );
            }

            col = col.child(row);
        }
        col
    }

    /// Taller editor chrome for the brief field (Enter inserts a newline).
    fn render_multiline_input(
        &self,
        hx: &HarnessTheme,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .key_context("HarnessInput")
            .on_action(cx.listener(Self::cancel_edit))
            .flex()
            .flex_col()
            .w_full()
            .min_h(px(72.0))
            .max_h(px(160.0))
            .px_1p5()
            .py_1()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.accent)
            .text_sm()
            .text_color(hx.tx0)
            .child(editor)
    }
}
