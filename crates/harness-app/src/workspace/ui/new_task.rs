//! Centered "New task" dialog — name, brief, optional branch, project members —
//! and its single-project "New worktree" mode, opened from a PROJECTS row.

use super::super::*;
use super::{modal_button, modal_control, toggled};

impl HarnessWorkspace {
    /// Push `feat/{slug}` into the branch field placeholder when the name changes.
    pub(crate) fn sync_new_task_branch_placeholder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let auto = self.new_task.as_ref().map(|dialog| {
            let name = dialog.name.read(cx).text(cx);
            format!("feat/{}", task_name_slug(name.trim()))
        });
        let Some(auto) = auto else { return };
        let Some(dialog) = self.new_task.as_mut() else {
            return;
        };
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
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(dialog) = self.new_task.as_ref() else {
            return div().into_any_element();
        };

        // Worktree mode: opened from a PROJECTS row, for that project only.
        let worktree_for = dialog.worktree_for.clone();
        let is_worktree = worktree_for.is_some();
        let title = if is_worktree {
            "New worktree"
        } else {
            "New task"
        };
        let name_error = dialog.name_required.then_some("Enter a name.");

        let field_label = |label: &str| {
            div()
                .text_xs()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(hx.tx2)
                .child(label.to_string())
        };

        let card = dialog.focus.card(div().id("new-task-card"), title);
        let card =
            card.w(px(480.0).min(window.viewport_size().width - px(24.0)))
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
                                .child(title),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(self.render_new_task_info(hx, is_worktree))
                                .child(
                                    modal_button("new-task-close", "Close", hx)
                                        .px_2()
                                        .rounded_md()
                                        .text_sm()
                                        .text_color(hx.tx2)
                                        .cursor_pointer()
                                        .hover(|d| d.bg(hx.surface).text_color(hx.tx0))
                                        .child("✕")
                                        .on_click(cx.listener(|ws, _, window, cx| {
                                            ws.close_new_task(window, cx);
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
                        .child(task_input("Name", &dialog.name, false, name_error, hx, cx))
                        .when_some(name_error, |d, error| {
                            d.child(
                                div()
                                    .id("new-task-name-error")
                                    .role(gpui::Role::Alert)
                                    .aria_label(error)
                                    .text_xs()
                                    .text_color(hx.red)
                                    .child(error),
                            )
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(field_label(if is_worktree {
                            "Brief (optional)"
                        } else {
                            "Brief"
                        }))
                        .child(task_input("Brief", &dialog.brief, true, None, hx, cx))
                        .child(div().text_size(px(10.0)).text_color(hx.tx2).child(
                            if is_worktree {
                                "Given to the agent as its starting prompt."
                            } else {
                                "Shared with every agent as the starting prompt."
                            },
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(field_label("Branch"))
                        .child(task_input("Branch", &dialog.branch, false, None, hx, cx))
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
                            modal_button("new-task-cancel", "Cancel", hx)
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
                                .on_click(
                                    cx.listener(|ws, _, window, cx| ws.close_new_task(window, cx)),
                                ),
                        )
                        .child(
                            modal_button(
                                "new-task-create",
                                if is_worktree {
                                    "Create worktree"
                                } else {
                                    "Create"
                                },
                                hx,
                            )
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
                            .child(if is_worktree {
                                "Create worktree"
                            } else {
                                "Create"
                            })
                            .on_click(cx.listener(
                                |ws, _, window, cx| {
                                    ws.submit_new_task(window, cx);
                                },
                            )),
                        ),
                )
                .child(dialog.focus.end());

        div()
            .id("new-task-backdrop")
            // Occlude, not just catch clicks: without it the elements under
            // an overlay still hover, and their tooltips show through it.
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_scroll(|_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|ws, _, window, cx| {
                ws.close_new_task(window, cx);
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
                "Name labels the effort. Brief is the starting prompt injected into agents. Multi-project tasks get a coordinator agent that delegates to each project's agent over A2A — project AGENTS.md is never overwritten."
            }))
            .child("ⓘ")
    }

    fn render_new_task_members(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // One row per project, previewing the post-create member list: checkbox
        // for include, and a Home radio only while a single project is in. With
        // two or more the task's coordinator leads and every repo is a peer, so
        // there is no home to pick (the first checked one is recorded).
        let multi = self.new_task_repos.len() >= 2;
        let mut col = div()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(hx.border)
            .pl_1();
        for (i, repo) in self.data.repos.iter().enumerate() {
            let name = repo.name.clone();
            let included = self.new_task_repos.iter().any(|n| n == &name);
            let is_home = included && !multi && self.new_task_home == name;
            let check = if included { "☑" } else { "☐" };
            let label = repo.name.clone();
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
                modal_control(
                    ("new-task-check", i),
                    label.clone(),
                    gpui::Role::CheckBox,
                    hx,
                )
                .aria_toggled(toggled(included))
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

            if included && !multi {
                let radio = if is_home { "●" } else { "○" };
                row = row.child(
                    modal_control(
                        ("new-task-home", i),
                        format!("{} as home project", name),
                        gpui::Role::RadioButton,
                        hx,
                    )
                    .aria_toggled(toggled(is_home))
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
}

fn task_input(
    label: &'static str,
    editor: &Entity<Editor>,
    multiline: bool,
    error: Option<&'static str>,
    hx: &HarnessTheme,
    cx: &App,
) -> gpui::Stateful<gpui::Div> {
    let focus = editor.focus_handle(cx).tab_stop(true);
    let border = if error.is_some() { hx.red } else { hx.border2 };
    div()
        .id(label)
        .role(if multiline {
            gpui::Role::MultilineTextInput
        } else {
            gpui::Role::TextInput
        })
        .aria_label(label)
        .aria_value(editor.read(cx).text(cx))
        .when_some(error, |d, error| d.aria_description(error))
        .track_focus(&focus)
        .key_context("HarnessInput")
        .flex()
        .w_full()
        .when(multiline, |d| {
            d.flex_col().min_h(px(72.0)).max_h(px(160.0)).py_1()
        })
        .when(!multiline, |d| d.items_center().h(px(22.0)))
        .px_1p5()
        .rounded_md()
        .bg(hx.bg0)
        .border_1()
        .border_color(border)
        .focus(|d| d.border_color(if error.is_some() { hx.red } else { hx.accent }))
        .text_sm()
        .text_color(hx.tx0)
        .child(editor.clone())
}
