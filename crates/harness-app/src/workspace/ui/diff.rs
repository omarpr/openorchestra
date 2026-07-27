//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

impl HarnessWorkspace {

    pub(crate) fn render_status_bar(
        &self,
        hx: &HarnessTheme,
        r: Option<&Resolved>,
        cx: &App,
    ) -> impl IntoElement {
        let branch = r.map(|r| r.branch.clone()).unwrap_or_default();
        // N2: real LSP state, replacing the placeholder. Reports the language
        // servers actually running on this worktree's Project, so "rust-analyzer"
        // appearing means it started — not that we hope it did.
        let lsp = r
            .and_then(|r| self.panes.get(&r.key))
            .and_then(|p| {
                p.tabs.iter().find_map(|t| match &t.content {
                    TabContent::Editor(ws) => Some(ws),
                    _ => None,
                })
            })
            .map(|ws| {
                let project = ws.read(cx).project().read(cx);
                let names: Vec<String> = project
                    .language_server_statuses(cx)
                    .map(|(_, status)| status.name.to_string())
                    .collect();
                if names.is_empty() {
                    "no LSP".to_string()
                } else {
                    names.join(", ")
                }
            })
            .unwrap_or_else(|| "LSP: open an editor tab".to_string());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_3()
            .h(px(24.0))
            .px_3()
            .bg(hx.bg1)
            .border_t_1()
            .border_color(hx.border)
            .text_xs()
            .child(div().font_family(hx.font_mono).text_color(hx.tx1).child(branch))
            .child(div().flex_1())
            .child(div().text_color(hx.tx2).child(self.note.clone()))
            .child(div().font_family(hx.font_mono).text_color(hx.tx2).child(lsp))
            .child(tool_chip(hx, "git", self.tools.git, true))
            .child(tool_chip(hx, "gh", self.tools.gh, false))
            .child(tool_chip(hx, "pi", self.tools.pi, false))
    }

    /// The unified-diff block for one file.
    ///
    /// `path` is the panel's identity key: a worktree-relative path, or
    /// `repo/rel` for a unified task diff. Git headers only carry `rel`, so
    /// unified keys are resolved back to the member-relative path first.
    pub(crate) fn unified_for_file(&self, path: &str) -> String {
        let lookup = self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == path)
            .map(|f| f.file.path.as_str())
            .unwrap_or(path);
        let mut blocks: Vec<String> = Vec::new();
        let mut cur = String::new();
        for line in self.diff.unified.lines() {
            if line.starts_with("diff --git") && !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur));
            }
            cur.push_str(line);
            cur.push('\n');
        }
        if !cur.is_empty() {
            blocks.push(cur);
        }
        blocks
            .into_iter()
            .find(|b| {
                b.lines().next().is_some_and(|h| {
                    h.contains(&format!("b/{lookup}")) || h.ends_with(lookup)
                })
            })
            .unwrap_or_default()
    }

    /// The review toolbar above the file list: how many comments there are,
    /// and what can be done with them.
    ///
    /// Shown only when there is something to act on. Adding a comment is a
    /// click on a line's gutter number in the diff itself, so this bar is
    /// purely about what to do with the comments you already have.
    pub(crate) fn render_review_bar(
        &self,
        hx: &HarnessTheme,
        count: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chip = |id: &'static str, label: String, color: Rgba| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(20.0))
                .px_1p5()
                .rounded_md()
                .text_xs()
                .text_color(color)
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface))
                .child(label)
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .px_3()
            .pb_1()
            .font_family(hx.font_mono)
            .child(div().flex_1())
            .child(div().flex_1())
            .child(
                    div()
                        .text_xs()
                        .text_color(hx.amber)
                        .child(format!("{count} comment{}", if count == 1 { "" } else { "s" })),
                )
            .child(chip("copy-review", "copy".to_string(), hx.tx2).on_click(
                cx.listener(|ws, _, _, cx| ws.copy_review(cx)),
            ))
            .child(chip("save-review", "save .md".to_string(), hx.tx2).on_click(
                cx.listener(|ws, _, _, cx| ws.save_review_markdown(cx)),
            ))
            .child(
                chip(
                        "clear-review",
                        if self.clearing_review { "discard?".to_string() } else { "clear".to_string() },
                        if self.clearing_review { hx.red } else { hx.tx2 },
                    )
                    .on_click(cx.listener(|ws, _, _, cx| ws.clear_review(cx))),
            )
    }

    /// The unified diff body, rendered inline beneath the file it belongs
    /// to (or beneath the whole list when no single file is selected).
    pub(crate) fn render_diff_body(&self, hx: &HarnessTheme, file: &str, cx: &mut Context<Self>) -> AnyElement {
        let mut preview = div()
            .id("diff-preview")
            .flex()
            .flex_col()
            .flex_none()
            .my_1()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.border)
            .items_start()
            .text_xs()
            .font_family(hx.font_mono);
        let unified = self.unified_for_file(file);
        if unified.is_empty() {
            preview = preview.child(div().p_2().text_color(hx.tx2).child("—"));
        } else {
            let shown = readable_diff_lines(&unified);
            let total = shown.len();
            let transparent = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
            // Comments hang off (file, line); with no file selected the rows
            // span several files, so fall back to each row's own header.
            let worktree_key = self.resolve().map(|r| r.key);
            let file_for_comments = file.to_string();
            for (ix, r) in shown.iter().enumerate().take(MAX_DIFF_LINES) {
                // A hunk boundary is a thin rule plus git's section text (the
                // enclosing function or heading). The old `@@ -a,b +c,d @@`
                // coordinates are gone: the gutter shows the real line numbers,
                // which is the same information without the decoding step.
                if r.kind == DiffLineKind::HunkBreak {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_2()
                            .h(px(18.0))
                            .px_2()
                            .border_t_1()
                            .border_color(hx.border)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_color(hx.tx2)
                                    .whitespace_nowrap()
                                    .child(r.text.clone()),
                            ),
                    );
                    continue;
                }

                let (fg, bg, marker) = match r.kind {
                    DiffLineKind::FileHeader => (hx.tx0, hx.surface, ""),
                    DiffLineKind::Added => (rgb(DIFF_ADD_TX), tint(hx.green, 0.13), "+"),
                    DiffLineKind::Removed => (rgb(DIFF_DEL_TX), tint(hx.red, 0.13), "−"),
                    DiffLineKind::Context => (hx.tx2, transparent, " "),
                    DiffLineKind::HunkBreak => unreachable!("handled above"),
                };

                let selected = self
                    .diff_selection
                    .as_ref()
                    .is_some_and(|(f, from, to)| f == file && ix >= *from && ix <= *to);
                let row_bg = if selected { tint(hx.accent, 0.22) } else { bg };

                if r.kind == DiffLineKind::FileHeader {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .h(px(15.0))
                            .bg(row_bg)
                            .px_2()
                            .text_color(fg)
                            .whitespace_nowrap()
                            .child(r.text.clone()),
                    );
                    continue;
                }

                let line_no = r.number;
                let comment_key = (file_for_comments.clone(), line_no);
                let row_file = file_for_comments.clone();
                let move_file = file_for_comments.clone();
                let has_comment = line_no.is_some_and(|n| {
                    worktree_key
                        .as_deref()
                        .is_some_and(|k| self.review.get(k, &file_for_comments, n).is_some())
                });

                preview = preview.child(
                    div()
                        .id(("diffrow", ix))
                        .flex()
                        .flex_none()
                        .h(px(15.0))
                        .bg(row_bg)
                        .cursor_pointer()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |ws, ev: &MouseDownEvent, _, cx| {
                                ws.diff_selecting = true;
                                ws.select_diff_row(&row_file, ix, ev.modifiers.shift, cx);
                            }),
                        )
                        // Dragging over rows extends the selection, which is
                        // what "highlight some lines" means with a mouse.
                        .on_mouse_move(cx.listener(move |ws, _: &MouseMoveEvent, _, cx| {
                            if ws.diff_selecting {
                                ws.extend_diff_selection(&move_file, ix, cx);
                            }
                        }))
                        .child(
                            // Fixed-width gutter so the code column stays
                            // aligned as numbers grow past 999. Doubles as the
                            // comment affordance.
                            div()
                                .id(("diffgutter", ix))
                                .flex_none()
                                .w(px(46.0))
                                .pr_1()
                                .flex()
                                .items_center()
                                .gap_1()
                                .when(has_comment, |d| d.bg(tint(hx.amber, 0.18)))
                                .text_color(if has_comment { hx.amber } else { hx.tx2 })
                                .cursor_pointer()
                                .tooltip(tip(if has_comment { "Edit comment" } else { "Comment on line" }))
                                .hover(|d| d.bg(hx.surface).text_color(hx.accent))
                                // The marker sits in a fixed slot rather than
                                // being prefixed to the number, so commented
                                // lines do not shift their digits sideways.
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(8.0))
                                        .child(if has_comment { "▸" } else { "" }),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .child(line_no.map(|n| n.to_string()).unwrap_or_default()),
                                )
                                .on_click(cx.listener(
                                    move |ws, ev: &gpui::ClickEvent, window, cx| {
                                        cx.stop_propagation();
                                        if let Some(line) = comment_key.1 {
                                            ws.begin_comment(
                                                comment_key.0.clone(),
                                                line,
                                                ev.position(),
                                                window,
                                                cx,
                                            );
                                        }
                                    },
                                )),
                        )
                        .child(div().flex_none().w(px(10.0)).text_color(fg).child(marker))
                        .child(
                            div()
                                .flex_none()
                                .pr_2()
                                .text_color(fg)
                                .whitespace_nowrap()
                                .child(r.text.clone()),
                        ),
                );

                // An existing note renders under its line, where the thing it
                // is about is still on screen.
                if has_comment
                    && let (Some(k), Some(n)) = (worktree_key.as_deref(), line_no)
                    && let Some(c) = self.review.get(k, &file_for_comments, n)
                {
                    preview = preview.child(
                        div()
                            .flex()
                            .flex_none()
                            .py_0p5()
                            .pl(px(56.0))
                            .pr_2()
                            .bg(tint(hx.amber, 0.10))
                            .border_l_2()
                            .border_color(hx.amber)
                            .text_color(hx.tx1)
                            .child(c.body.clone()),
                    );
                }
            }
            if total > MAX_DIFF_LINES {
                // Never truncate silently — a diff that just stops looks like
                // the change is smaller than it is.
                preview = preview.child(
                    div()
                        .flex_none()
                        .h(px(15.0))
                        .px_2()
                        .text_color(hx.amber)
                        .child(format!(
                            "… {} more lines — select a single file, or view the full diff in git",
                            total - MAX_DIFF_LINES
                        )),
                );
            }
        }
        preview.into_any_element()
    }

    pub(crate) fn render_diff_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        let Some(r) = self.resolve() else {
            return div()
                .flex()
                .flex_col()
                .flex_none()
                .w(px(self.prefs.diff_width))
                .h_full()
                .items_center()
                .justify_center()
                .gap_2()
                .px_3()
                .bg(hx.bg1)
                .border_l_1()
                .border_color(hx.border)
                .child(div().text_sm().text_color(hx.tx1).child("No project selected"))
                .child(
                    div()
                        .text_xs()
                        .text_color(hx.tx2)
                        .child("There is no project selected or created. Create your first one!"),
                )
                .into_any_element();
        };
        let (adds, dels) = self.diff.totals();
        let review_count = self.resolve().map_or(0, |r| self.review.count(&r.key));

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.diff_width))
            .h_full()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2()
                    .h(px(34.0))
                    .px_3()
                    .border_b_1()
                    .border_color(hx.border)
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child("DIFF"))
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(r.branch.clone()))
                    ,
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_between()
                    .px_3()
                    .py_2()
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(
                        if self.diff_pending {
                            "Changed files  …".to_string()
                        } else {
                            let total = self.diff.files.len();
                            let done = self.resolve().map_or(0, |r| {
                                self.review.reviewed_count(
                                    &r.key,
                                    self.diff.files.iter().map(|f| f.path.as_str()),
                                )
                            });
                            if done > 0 {
                                format!("Changed files  {done}/{total} reviewed")
                            } else {
                                format!("Changed files  {total}")
                            }
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_mono)
                            .child(div().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            );

        // One scroller holding the file list, with the selected file's diff
        // expanded directly beneath its row — so the diff is next to the name
        // it belongs to instead of in a separate pane you have to look away to.
        let worktree_key = self.resolve().map(|r| r.key);
        let mut files = div()
            .id("diff-files")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .items_start()
            .overflow_scroll()
            .px_2()
            .pb_1();
        if self.diff.files.is_empty() {
            files = files.child(div().px_2().py_1().text_xs().text_color(hx.tx2).child("no changes vs merge-base"));
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
                files = files.child(self.render_diff_file_row(
                    hx,
                    i,
                    &key,
                    &uf.file.path,
                    &uf.file,
                    worktree_key.as_deref(),
                    cx,
                ));
                if self.expanded.contains(&key) {
                    files = files.child(self.render_diff_file_body(hx, &key, cx));
                }
            }
        } else {
            for (i, file) in self.diff.files.iter().enumerate() {
                let path = file.path.clone();
                files = files.child(self.render_diff_file_row(
                    hx,
                    i,
                    &path,
                    &path,
                    file,
                    worktree_key.as_deref(),
                    cx,
                ));
                if self.expanded.contains(&path) {
                    files = files.child(self.render_diff_file_body(hx, &path, cx));
                }
            }
        }

        if review_count > 0 {
            panel = panel.child(self.render_review_bar(hx, review_count, cx));
        }
        panel = panel.child(files);

        panel.into_any_element()

    }

    /// One changed-file row: checkbox, edit button, disclosure, badge, path,
    /// +/- counts. `key` is the stable identity (reviewed / expanded / open);
    /// `display_path` is what the row shows (relative under a project group).
    fn render_diff_file_row(
        &self,
        hx: &HarnessTheme,
        i: usize,
        key: &str,
        display_path: &str,
        file: &worktrees::DiffFile,
        worktree_key: Option<&str>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let expanded = self.expanded.contains(key);
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

        div()
            .id(("difffile", i))
            .flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_md()
            .when(expanded, |d| d.bg(hx.surface))
            .cursor_pointer()
            .hover(|d| d.bg(hx.surface))
            .on_click(cx.listener(move |ws, _, _, cx| {
                if !ws.expanded.remove(&toggle_path) {
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
                    .font_family(hx.font_mono)
                    .text_color(if reviewed { hx.green } else { hx.tx2 })
                    .cursor_pointer()
                    .tooltip(tip(if reviewed { "Mark not reviewed" } else { "Mark reviewed" }))
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
                        ::ui::Icon::new(::ui::IconName::File)
                            .size(::ui::IconSize::Indicator),
                    )
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.open_diff_file(&edit_path, window, cx);
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(8.0))
                    .text_xs()
                    .text_color(hx.tx2)
                    .child(if expanded { "▾" } else { "▸" }),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(12.0))
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(badge_color)
                    .child(file.change.badge().to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(if reviewed { hx.tx2 } else { hx.tx1 })
                    .overflow_hidden()
                    .child(display_path.to_string()),
            )
            .when(expanded && crate::markdown_preview::is_markdown(&md_check), |d| {
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
            })
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.green)
                    .child(format!("+{}", file.adds)),
            )
            .child(
                div()
                    .flex_none()
                    .text_xs()
                    .font_family(hx.font_mono)
                    .text_color(hx.red)
                    .child(format!("−{}", file.dels)),
            )
            .into_any_element()
    }

    /// Inline diff body (or markdown preview) under an expanded file row.
    fn render_diff_file_body(
        &self,
        hx: &HarnessTheme,
        key: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.md_preview.as_ref().filter(|(f, _)| f == key) {
            Some((_, md)) => div()
                .w_full()
                .h(px(360.0))
                .my_1()
                .rounded_md()
                .bg(hx.bg0)
                .border_1()
                .border_color(hx.border)
                .child(md.clone())
                .into_any_element(),
            None => self.render_diff_body(hx, key, cx),
        }
    }
}
