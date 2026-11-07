//! Right-hand tools panel: Files / Git / Review as tabs.
//!
//! Files hosts Explorer (Zed `ProjectPanel`) and Search (harness project
//! find-in-files). Git hosts Zed's `GitPanel`. Review is the harness diff
//! surface. One column replaces the old left-dock + right-DIFF split.

use super::super::*;

impl HarnessWorkspace {
    /// The docked tools column: tab strip + active surface.
    pub(crate) fn render_tool_panel(
        &self,
        hx: &HarnessTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.diff_width))
            .h_full()
            .min_h_0()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(self.render_tool_tab_strip(hx, cx))
            .when(self.prefs.tool_tab == ToolTab::Files, |d| {
                d.child(self.render_files_sub_tab_strip(hx, cx))
            })
            .child(self.render_tool_body(hx, window, cx))
            .into_any_element()
    }

    fn render_tool_tab_strip(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.prefs.tool_tab;
        let review_count = self
            .resolve()
            .map(|r| self.review.count(&r.key))
            .unwrap_or(0);
        let dirty = self.selection_dirty_count();

        let mut strip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(34.0))
            .px_2()
            .border_b_1()
            .border_color(hx.border);

        for tab in [ToolTab::Files, ToolTab::Git, ToolTab::Review] {
            let selected = active == tab;
            let label = match tab {
                ToolTab::Files => "Files".to_string(),
                ToolTab::Git if dirty > 0 => format!("Git · {dirty}"),
                ToolTab::Git => "Git".to_string(),
                ToolTab::Review if review_count > 0 => format!("Review · {review_count}"),
                ToolTab::Review => "Review".to_string(),
            };
            strip = strip.child(
                div()
                    .id(gpui::SharedString::from(format!("tool-tab-{}", tab.as_str())))
                    .flex()
                    .items_center()
                    .h(px(24.0))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .cursor_pointer()
                    .when(selected, |d| d.bg(hx.surface2).text_color(hx.tx0))
                    .when(!selected, |d| {
                        d.text_color(hx.tx2)
                            .hover(|h| h.bg(hx.surface).text_color(hx.tx1))
                    })
                    .child(label)
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.set_tool_tab(tab, window, cx);
                    })),
            );
        }

        strip = strip.child(div().flex_1());

        // Review actions live in the tab strip so they don't fight the tabs.
        if active == ToolTab::Review && review_count > 0 {
            strip = strip.child(self.render_review_bar(hx, review_count, cx));
        }

        strip
    }

    /// Explorer | Search under the Files tool tab.
    fn render_files_sub_tab_strip(
        &self,
        hx: &HarnessTheme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.files_sub_tab;
        let mut strip = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(28.0))
            .px_2()
            .border_b_1()
            .border_color(hx.border);

        for (tab, label) in [
            (FilesSubTab::Explorer, "Explorer"),
            (FilesSubTab::Search, "Search"),
        ] {
            let selected = active == tab;
            strip = strip.child(
                div()
                    .id(gpui::SharedString::from(format!(
                        "files-sub-{}",
                        match tab {
                            FilesSubTab::Explorer => "explorer",
                            FilesSubTab::Search => "search",
                        }
                    )))
                    .flex()
                    .items_center()
                    .h(px(22.0))
                    .px_2()
                    .rounded_md()
                    .text_xs()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(hx.surface).text_color(hx.tx0))
                    .when(!selected, |d| {
                        d.text_color(hx.tx2)
                            .hover(|h| h.bg(hx.surface_hover).text_color(hx.tx1))
                    })
                    .child(label)
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        ws.set_files_sub_tab(tab, window, cx);
                    })),
            );
        }
        strip
    }

    pub(crate) fn set_files_sub_tab(
        &mut self,
        tab: FilesSubTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.files_sub_tab = tab;
        self.prefs.tool_tab = ToolTab::Files;
        self.prefs.diff_open = true;
        match tab {
            FilesSubTab::Explorer => {
                let _ = self.ensure_service_workspace(window, cx);
                self.activate_hosted_tool_panel(ToolTab::Files, window, cx);
            }
            FilesSubTab::Search => {
                let _ = self.ensure_service_workspace(window, cx);
                self.ensure_project_search_ui(window, cx);
                if let Some(ui) = self.project_search.as_ref() {
                    window.focus(&ui.query.focus_handle(cx), cx);
                }
            }
        }
        self.save_prefs(cx);
        cx.notify();
    }

    fn render_tool_body(
        &self,
        hx: &HarnessTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.prefs.tool_tab {
            ToolTab::Review if self.prefs.diff_location == DiffLocation::Tab => {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .px_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(hx.tx1)
                            .child("Review is a center tab"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(hx.tx2)
                            .child("Open it from ＋, or switch Review location to Tools panel."),
                    )
                    .child(
                        div()
                            .id("tool-open-review-tab")
                            .mt_2()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(hx.surface)
                            .text_xs()
                            .text_color(hx.tx0)
                            .cursor_pointer()
                            .hover(|d| d.bg(hx.surface_hover))
                            .child("Open Review tab")
                            .on_click(cx.listener(|ws, _, window, cx| {
                                ws.open_tab(TabKind::Diff, window, cx);
                            })),
                    )
                    .into_any_element()
            }
            ToolTab::Review => {
                // Skip the inner review bar — actions are in the tab strip.
                self.render_diff_surface(hx, Some(self.prefs.diff_width), false, cx)
            }
            ToolTab::Files if self.files_sub_tab == FilesSubTab::Search => {
                self.render_project_search_panel(hx, cx)
            }
            ToolTab::Files => self.render_hosted_panel::<project_panel::ProjectPanel>(
                hx,
                "Browse files from the tools panel",
                window,
                cx,
            ),
            ToolTab::Git => self.render_hosted_panel::<git_ui::git_panel::GitPanel>(
                hx,
                "Use git from the tools panel",
                window,
                cx,
            ),
        }
    }

    /// Render a Zed dock panel inside harness chrome, with docks kept closed.
    fn render_hosted_panel<P: workspace::Panel + 'static>(
        &self,
        hx: &HarnessTheme,
        empty_hint: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(hosted) = self.editor_workspace() else {
            return self
                .render_tool_empty(
                    hx,
                    empty_hint,
                    "The file tree and git panel load with the project workspace.",
                    cx,
                )
                .into_any_element();
        };

        // Keep Zed docks closed so the editor stays full-bleed; we host the
        // panel views ourselves in this column.
        hosted.update(cx, |ws, cx| {
            for dock in ws.all_docks() {
                if dock.read(cx).is_open() {
                    dock.update(cx, |dock, cx| dock.set_open(false, window, cx));
                }
            }
        });

        let Some(panel) = hosted.read(cx).panel::<P>(cx) else {
            return div()
                .flex()
                .flex_1()
                .h_full()
                .min_h_0()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("Loading…")
                .into_any_element();
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_h_0()
            .min_w_0()
            .size_full()
            .child(panel.clone())
            .into_any_element()
    }

    fn render_tool_empty(
        &self,
        hx: &HarnessTheme,
        title: &'static str,
        detail: &'static str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .items_center()
            .justify_center()
            .gap_2()
            .px_3()
            .child(div().text_sm().text_color(hx.tx1).child(title))
            .child(div().text_xs().text_color(hx.tx2).child(detail))
            .child(
                div()
                    .id("tool-open-editor")
                    .mt_2()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(hx.surface)
                    .text_xs()
                    .text_color(hx.tx0)
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface_hover))
                    .child("Open editor")
                    .on_click(cx.listener(|ws, _, window, cx| {
                        ws.ensure_editor_workspace_quiet(window, cx);
                        cx.notify();
                    })),
            )
    }

    /// Dirty-file count for the current selection (sidebar git watch).
    fn selection_dirty_count(&self) -> usize {
        match &self.selection {
            Selection::Main(name) => self
                .data
                .repos
                .iter()
                .find(|r| &r.name == name)
                .map(|r| r.dirty)
                .unwrap_or(0),
            Selection::Worktree(id) => self
                .data
                .tasks
                .iter()
                .flat_map(|t| t.members.iter())
                .find(|m| &m.worktree_id == id)
                .map(|m| m.dirty)
                .unwrap_or(0),
            Selection::Task { task_id, member_id } => {
                let Some(task) = self.data.tasks.iter().find(|t| &t.id == task_id) else {
                    return 0;
                };
                if let Some(mid) = member_id {
                    task.members
                        .iter()
                        .find(|m| &m.worktree_id == mid)
                        .map(|m| m.dirty)
                        .unwrap_or(0)
                } else {
                    task.dirty
                }
            }
        }
    }
}

impl ToolTab {
    fn as_str(self) -> &'static str {
        match self {
            ToolTab::Files => "files",
            ToolTab::Git => "git",
            ToolTab::Review => "review",
        }
    }
}
