//! The changed-files list and the focused single-file view.

use super::*;

impl HarnessWorkspace {
    /// The changed-file list.
    ///
    /// In two-pane it is the left column and a click *selects*; in the narrow
    /// layout it is the whole surface and a click *expands* the file's diff
    /// beneath its own row, so the diff stays next to the name it belongs to.
    pub(crate) fn render_file_list(
        &self,
        hx: &HarnessTheme,
        two_pane: bool,
        body_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let worktree_key = self.resolve().map(|r| r.key);
        let mut files = div()
            .id("diff-files")
            .flex()
            .flex_col()
            .min_h_0()
            .items_start()
            .overflow_scroll()
            .track_scroll(&self.diff_files_scroll)
            .px_2()
            .pb_1();
        files = if two_pane {
            files.flex_none().w(px(self.prefs.diff_list_width))
        } else {
            files.flex_1()
        };
        if self.diff.files.is_empty() {
            files = files.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("no changes vs merge-base"),
            );
        } else if !self.unified_diff.files.is_empty() {
            // Unified task diff: group by project instead of prefixing every
            // path with the repo name.
            let mut last_repo: Option<&str> = None;
            for (i, uf) in self.unified_diff.files.iter().enumerate() {
                if last_repo != Some(uf.repo.as_str()) {
                    last_repo = Some(&uf.repo);
                    files = files.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .w_full()
                            .px_2()
                            .pt_2()
                            .pb_1()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(hx.tx2)
                                    .child(uf.repo.clone()),
                            ),
                    );
                }
                let key = format!("{}/{}", uf.repo, uf.file.path);
                files = self.append_diff_file_row(
                    files,
                    hx,
                    i,
                    &key,
                    &uf.file.path,
                    &uf.file,
                    worktree_key.as_deref(),
                    two_pane,
                    body_width,
                    cx,
                );
            }
        } else {
            for (i, file) in self.diff.files.iter().enumerate() {
                let path = file.path.clone();
                files = self.append_diff_file_row(
                    files,
                    hx,
                    i,
                    &path,
                    &path,
                    file,
                    worktree_key.as_deref(),
                    two_pane,
                    body_width,
                    cx,
                );
            }
        }

        files.into_any_element()
    }

    /// Append one changed-file row (and, in accordion mode, its body) to the
    /// file list. `key` is the stable identity; `display_path` is what the row
    /// shows (relative under a project group for unified diffs).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn append_diff_file_row(
        &self,
        mut files: gpui::Stateful<gpui::Div>,
        hx: &HarnessTheme,
        i: usize,
        key: &str,
        display_path: &str,
        file: &worktrees::DiffFile,
        worktree_key: Option<&str>,
        two_pane: bool,
        body_width: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let expanded = self.expanded.contains(key);
        let open = if two_pane {
            self.focused_file().as_deref() == Some(key)
        } else {
            expanded
        };
        let reviewed = worktree_key.is_some_and(|k| self.review.is_reviewed(k, key));
        let badge_color = match file.change {
            Change::Added => hx.green,
            Change::Deleted => hx.red,
            Change::Renamed => hx.accent,
            _ => hx.amber,
        };
        let check_path = key.to_string();
        let toggle_path = key.to_string();
        let edit_path = key.to_string();
        let md_path = key.to_string();
        let md_check = display_path.to_string();
        let body_key = key.to_string();

        files = files.child(
            div()
                .id(("difffile", i))
                .flex()
                .w_full()
                .items_center()
                .gap_2()
                .px_2()
                .py_1()
                .rounded_md()
                // The open file in the accent tint: the plain surface colour
                // it used to get was too close to the list's own background
                // to tell which file the diff beside it belonged to.
                .when(two_pane && open, |d| d.bg(tint(hx.accent, 0.2)))
                .when(!two_pane && open, |d| d.bg(hx.surface))
                .cursor_pointer()
                .when(!(two_pane && open), |d| d.hover(|d| d.bg(hx.surface)))
                .on_click(cx.listener(move |ws, _, window, cx| {
                    if two_pane {
                        ws.diff_focus = Some(toggle_path.clone());
                        // So ↑/↓ move through the list from here.
                        window.focus(&ws.diff_surface_focus, cx);
                    } else if !ws.expanded.remove(&toggle_path) {
                        ws.expanded.insert(toggle_path.clone());
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .id(("reviewed", i))
                        .flex_none()
                        .w(px(14.0))
                        .text_xs()
                        .font_family(hx.font_ui)
                        .text_color(if reviewed { hx.green } else { hx.tx2 })
                        .cursor_pointer()
                        .tooltip(tip(if reviewed {
                            "Mark not reviewed"
                        } else {
                            "Mark reviewed"
                        }))
                        .hover(|d| d.text_color(hx.green))
                        .child(if reviewed { "☑" } else { "☐" })
                        .on_click(cx.listener(move |ws, _, _, cx| {
                            cx.stop_propagation();
                            let Some(r) = ws.resolve() else { return };
                            let now = !ws.review.is_reviewed(&r.key, &check_path);
                            ws.review.set_reviewed(&r.key, &check_path, now);
                            ws.review.save(&ws.review_path);
                            if now {
                                ws.expanded.remove(&check_path);
                            }
                            cx.notify();
                        })),
                )
                // Open in editor — separate from the row click so expanding /
                // focusing the inline diff stays a pure review gesture.
                .child(
                    div()
                        .id(("edit-file", i))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(14.0))
                        .text_color(hx.tx2)
                        .cursor_pointer()
                        .tooltip(tip("edit file"))
                        .hover(|d| d.text_color(hx.accent))
                        .child(
                            ::ui::Icon::new(::ui::IconName::File).size(::ui::IconSize::Indicator),
                        )
                        .on_click(cx.listener(move |ws, _, window, cx| {
                            cx.stop_propagation();
                            ws.open_diff_file(&edit_path, window, cx);
                        })),
                )
                .when(!two_pane, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .w(px(8.0))
                            .text_xs()
                            .text_color(hx.tx2)
                            .child(if expanded { "▾" } else { "▸" }),
                    )
                })
                .child(
                    div()
                        .flex_none()
                        .w(px(12.0))
                        .text_xs()
                        .font_family(hx.font_ui)
                        .text_color(badge_color)
                        .child(file.change.badge().to_string()),
                )
                .child({
                    let label = div()
                        .flex_1()
                        .min_w_0()
                        // text_sm = Zed's default label size, so file names
                        // match the Files and Git panels beside this tab.
                        .text_sm()
                        .font_family(hx.font_ui)
                        .text_color(if two_pane && open {
                            hx.tx0
                        } else if reviewed {
                            hx.tx2
                        } else {
                            hx.tx1
                        })
                        .whitespace_nowrap()
                        .overflow_hidden();
                    // The two-pane list is narrow by design, and middle-ellipsing
                    // a deep path there left every row reading "crate…rs". Lead
                    // with the file name and let the dimmed directory be what
                    // gets cut, as zed's own git panel does.
                    match display_path.rsplit_once('/') {
                        Some((dir, name)) if two_pane => label
                            .flex()
                            .gap_1()
                            .child(div().flex_none().child(name.to_string()))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(hx.tx2)
                                    .child(dir.to_string()),
                            ),
                        _ => label.text_ellipsis_middle().child(display_path.to_string()),
                    }
                })
                .when(
                    open && crate::markdown_preview::is_markdown(&md_check),
                    |d| {
                        let previewing =
                            self.md_preview.as_ref().is_some_and(|(f, _)| f == &md_path);
                        let p = md_path.clone();
                        d.child(
                            div()
                                .id(("md-toggle", i))
                                .flex_none()
                                .px_1()
                                .rounded_md()
                                .text_xs()
                                .text_color(if previewing { hx.accent } else { hx.tx2 })
                                .cursor_pointer()
                                .tooltip(tip("Toggle Markdown preview"))
                                .hover(|h| h.bg(hx.surface_hover))
                                .child(if previewing { "◨" } else { "◧" })
                                .on_click(cx.listener(move |ws, _, _, cx| {
                                    cx.stop_propagation();
                                    ws.toggle_md_preview(&p, cx);
                                })),
                        )
                    },
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(hx.font_ui)
                        .text_color(hx.green)
                        .child(format!("+{}", file.adds)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_xs()
                        .font_family(hx.font_ui)
                        .text_color(hx.red)
                        .child(format!("−{}", file.dels)),
                ),
        );

        if expanded && !two_pane {
            match self.md_preview.as_ref().filter(|(f, _)| f == &body_key) {
                Some((_, md)) => {
                    files = files.child(
                        div()
                            .w_full()
                            .h(px(360.0))
                            .my_1()
                            .rounded_md()
                            .bg(hx.bg0)
                            .border_1()
                            .border_color(hx.border)
                            .child(md.clone()),
                    );
                }
                None => {
                    files = files.child(self.render_diff_body(hx, &body_key, true, body_width, cx))
                }
            }
        }
        files
    }

    /// ↑/↓ in the two-pane layout: show the previous or next changed file,
    /// in list order, and scroll the list to keep it in view. Stops at the
    /// ends rather than wrapping. Returns whether the key was used.
    pub(crate) fn step_diff_file(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let keys: Vec<&str> = self.diff.files.iter().map(|f| f.path.as_str()).collect();
        if keys.is_empty() {
            return false;
        }
        let current = self
            .focused_file()
            .and_then(|f| keys.iter().position(|k| *k == f))
            .unwrap_or(0);
        let next = if down {
            (current + 1).min(keys.len() - 1)
        } else {
            current.saturating_sub(1)
        };
        self.diff_focus = Some(keys[next].to_string());
        // The list's children include a heading before each project's files
        // in a multi-repo task, so the row's child index runs ahead of its
        // file index by the headings above it.
        let headings = if self.unified_diff.files.is_empty() {
            0
        } else {
            let mut seen: Option<&str> = None;
            self.unified_diff.files[..=next.min(self.unified_diff.files.len() - 1)]
                .iter()
                .filter(|uf| {
                    let new = seen != Some(uf.repo.as_str());
                    seen = Some(uf.repo.as_str());
                    new
                })
                .count()
        };
        self.diff_files_scroll.scroll_to_item(next + headings);
        cx.notify();
        true
    }

    /// Which file the two-pane layout is showing.
    ///
    /// Falls back to the first changed file so the right column is never blank
    /// while there is something to review — an empty pane beside a populated
    /// list reads as a failure to load rather than as "nothing selected".
    pub(crate) fn focused_file(&self) -> Option<String> {
        self.diff_focus
            .clone()
            .filter(|f| self.diff.files.iter().any(|file| &file.path == f))
            .or_else(|| self.diff.files.first().map(|f| f.path.clone()))
    }

    /// The right-hand column: the focused file's diff, or its Markdown preview.
    pub(crate) fn render_focused_diff(
        &self,
        hx: &HarnessTheme,
        body_width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(file) = self.focused_file() else {
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(hx.tx2)
                .child("no changes vs merge-base")
                .into_any_element();
        };
        // The two branches need opposite containers, which is why this is not
        // one `div` with a `match` inside it.
        //
        // The markdown preview is a plain child that grows to its content, so it
        // needs an ancestor that scrolls. The diff body is a `uniform_list`,
        // which needs the exact reverse: gpui's own docs say it renders only the
        // visible subset "when rendered into a container with overflow-y: hidden
        // and a fixed (or max) height". Give it a scrolling ancestor instead and
        // its height resolves against its content rather than the pane — and a
        // virtualized list's content is one measured item, so the whole diff
        // collapses to a ~30px strip with the pane empty beneath it. It looks
        // like the diff failed to load; it is actually the list doing exactly
        // what an auto height asks of it.
        let container = div()
            .id("diff-focused")
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .min_w_0()
            .min_h_0()
            .p_1();
        match self.md_preview.as_ref().filter(|(f, _)| f == &file) {
            Some((_, md)) => container
                .overflow_scroll()
                .child(div().size_full().child(md.clone()))
                .into_any_element(),
            // Horizontal only: long lines still scroll sideways, while the
            // vertical axis stays hidden so the list keeps its definite height.
            None => container
                .overflow_x_scroll()
                .child(self.render_diff_body(hx, &file, false, body_width, cx))
                .into_any_element(),
        }
    }
}
