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
    pub(crate) fn unified_for_file(&self, path: &str) -> String {
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
            .find(|b| b.lines().next().is_some_and(|h| h.contains(&format!("b/{path}")) || h.ends_with(path)))
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
            .font_family(hx.font_mono)
            // Typing with a line selected is the other way people say "I want to
            // change this". Modified chords are shortcuts, not text — ⌘C over a
            // selection must still copy — so they pass straight through.
            .on_key_down(cx.listener(|ws, ev: &gpui::KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
                    return;
                }
                if k.key_char.as_deref().is_none_or(|c| c.trim().is_empty()) {
                    return;
                }
                let Some((file, from, _)) = ws.diff_selection.clone() else {
                    return;
                };
                let rows = readable_diff_lines(&ws.unified_for_file(&file));
                let line = target_line_for_row(&rows, from);
                ws.request_edit(file, line, window, cx);
            }));
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
            // Shared by every row's double-click handler: the parsed rows are
            // what turn a clicked index into a line that exists in the file.
            let rows_for_edit = std::sync::Arc::new(shown.clone());
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
                let dbl_file = file_for_comments.clone();
                let dbl_rows = rows_for_edit.clone();
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
                            cx.listener(move |ws, ev: &MouseDownEvent, window, cx| {
                                // A second click on a diff line means "let me
                                // change this". The diff itself cannot be typed
                                // into, so offer the file that can.
                                if ev.click_count >= 2 {
                                    ws.diff_selecting = false;
                                    let line = target_line_for_row(&dbl_rows, ix);
                                    ws.request_edit(dbl_file.clone(), line, window, cx);
                                    return;
                                }
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

    /// The dock: fixed width and its own left border, wrapping the shared
    /// surface. Everything inside is identical to the tab — only the chrome
    /// that makes it a *dock* lives here.
    pub(crate) fn render_diff_panel(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.prefs.diff_width))
            .h_full()
            .bg(hx.bg1)
            .border_l_1()
            .border_color(hx.border)
            .child(self.render_diff_surface(hx, Some(self.prefs.diff_width), cx))
            .into_any_element()
    }

    /// The diff review surface, drawn identically in the dock and in a tab.
    ///
    /// `dock_width` is `Some` for the dock and `None` for a tab — the only thing
    /// the two differ by, since a tab always has room for two columns.
    pub(crate) fn render_diff_surface(
        &self,
        hx: &HarnessTheme,
        dock_width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(r) = self.resolve() else {
            return self.render_no_project(hx).into_any_element();
        };
        let two_pane = use_two_pane(
            self.prefs.diff_location,
            dock_width.unwrap_or(f32::INFINITY),
        );
        let (adds, dels) = self.diff.totals();
        let review_count = self.review.count(&r.key);

        let mut panel = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            // Focusable so the surface can receive keystrokes: typing over a
            // selected line is one of the two edit-intent triggers, and gpui
            // routes key events along the focus path only.
            .track_focus(&self.diff_surface_focus)
            .child(self.render_diff_header(hx, &r, adds, dels, cx));
        if review_count > 0 {
            panel = panel.child(self.render_review_bar(hx, review_count, cx));
        }
        panel = if two_pane {
            panel.child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(self.render_file_list(hx, true, cx))
                    .child(self.splitter(hx, Drag::DiffList, cx))
                    .child(self.render_focused_diff(hx, cx)),
            )
        } else {
            panel.child(self.render_file_list(hx, false, cx))
        };
        panel.into_any_element()
    }

    /// Shown in place of the surface when nothing is selected.
    fn render_no_project(&self, hx: &HarnessTheme) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .items_center()
            .justify_center()
            .gap_2()
            .px_3()
            .child(div().text_sm().text_color(hx.tx1).child("No project selected"))
            .child(
                div()
                    .text_xs()
                    .text_color(hx.tx2)
                    .child("There is no project selected or created. Create your first one!"),
            )
    }

    /// The branch line and the changed-files/±counts line above the review.
    /// The push control, shown only when this branch actually has commits the
    /// remote does not.
    ///
    /// Two steps, like the review bar's clear/discard: the armed label names the
    /// remote and the count, because a button reading "push" alone does not say
    /// where the work is about to go — and for a branch with no upstream, where
    /// it goes is a remote branch that does not exist yet.
    fn render_push_control(
        &self,
        hx: &HarnessTheme,
        r: &Resolved,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let (path, state) = self.push_state.as_ref()?;
        // The probe is async; ignore an answer that outlived its selection.
        if path != &r.path || state.unpushed == 0 {
            return None;
        }
        let n = state.unpushed;
        let publishing = state.upstream.is_none();

        let (label, color) = if self.pushing {
            ("pushing…".to_string(), hx.tx2)
        } else if self.push_armed {
            let target = match &state.upstream {
                Some(u) => u.clone(),
                None => format!("origin/{}", r.branch),
            };
            let verb = if publishing { "publish" } else { "push" };
            (format!("{verb} {n} → {target}?"), hx.amber)
        } else if publishing {
            (format!("↑{n} publish"), hx.green)
        } else {
            (format!("↑{n} push"), hx.green)
        };

        Some(
            div()
                .id("push-branch")
                .flex()
                .items_center()
                .h(px(20.0))
                .px_1p5()
                .rounded_md()
                .text_xs()
                .font_family(hx.font_mono)
                .text_color(color)
                .when(!self.pushing, |d| {
                    d.cursor_pointer().hover(|d| d.bg(hx.surface))
                })
                .child(label)
                .on_click(cx.listener(|ws, _, _, cx| ws.push_branch(cx)))
                .into_any_element(),
        )
    }

    fn render_diff_header(
        &self,
        hx: &HarnessTheme,
        r: &Resolved,
        adds: usize,
        dels: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_none()
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
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_mono).text_color(hx.tx1).overflow_hidden().child(r.branch.clone())),
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
                            let done = self.review.reviewed_count(
                                &r.key,
                                self.diff.files.iter().map(|f| f.path.as_str()),
                            );
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
                            .children(self.render_push_control(hx, r, cx))
                            .child(div().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            )
    }

    /// The changed-file list.
    ///
    /// In two-pane it is the left column and a click *selects*; in the narrow
    /// layout it is the whole surface and a click *expands* the file's diff
    /// beneath its own row, so the diff stays next to the name it belongs to.
    fn render_file_list(&self, hx: &HarnessTheme, two_pane: bool, cx: &mut Context<Self>) -> AnyElement {
        let worktree_key = self.resolve().map(|r| r.key);
        let mut files = div()
            .id("diff-files")
            .flex()
            .flex_col()
            .min_h_0()
            .items_start()
            .overflow_scroll()
            .px_2()
            .pb_1();
        files = if two_pane {
            files.flex_none().w(px(self.prefs.diff_list_width))
        } else {
            files.flex_1()
        };
        if self.diff.files.is_empty() {
            files = files.child(div().px_2().py_1().text_xs().text_color(hx.tx2).child("no changes vs merge-base"));
        }
        for (i, file) in self.diff.files.iter().enumerate() {
            let path = file.path.clone();
            // Two-pane has one focused file and shows it beside the list;
            // the narrow layout expands any number of them in place.
            let expanded = self.expanded.contains(&file.path);
            let open = if two_pane {
                self.focused_file().as_deref() == Some(path.as_str())
            } else {
                expanded
            };
            let reviewed = worktree_key
                .as_deref()
                .is_some_and(|k| self.review.is_reviewed(k, &file.path));
            let badge_color = match file.change {
                Change::Added => hx.green,
                Change::Deleted => hx.red,
                Change::Renamed => hx.accent,
                _ => hx.amber,
            };
            let check_path = path.clone();
            let toggle_path = path.clone();
            let md_path = path.clone();

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
                    .when(open, |d| d.bg(hx.surface))
                    .cursor_pointer()
                    .hover(|d| d.bg(hx.surface))
                    .on_click(cx.listener(move |ws, _, _, cx| {
                        if two_pane {
                            ws.diff_focus = Some(toggle_path.clone());
                        } else if !ws.expanded.remove(&toggle_path) {
                            ws.expanded.insert(toggle_path.clone());
                        }
                        cx.notify();
                    }))
                    // Reviewed checkbox. Ticking it collapses the file: the
                    // point of marking it is to get it out of the way, and
                    // leaving it open would defeat that.
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
                    // Disclosure marker, so expanded state reads without
                    // relying on the row background alone. Two-pane has nothing
                    // to disclose — the diff is already beside the list — so it
                    // would only be a chevron that never means anything.
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
                    .child(div().flex_none().w(px(12.0)).text_xs().font_family(hx.font_mono).text_color(badge_color).child(file.change.badge().to_string()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(hx.font_mono)
                            // Reviewed files recede rather than vanish — still
                            // findable, visibly done.
                            .text_color(if reviewed { hx.tx2 } else { hx.tx1 })
                            .overflow_hidden()
                            .child(file.path.clone()),
                    )
                    // The markdown preview belongs to a file, so its toggle
                    // lives on that file's row, not the panel header.
                    .when(open && crate::markdown_preview::is_markdown(&file.path), |d| {
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
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.green).child(format!("+{}", file.adds)))
                    .child(div().flex_none().text_xs().font_family(hx.font_mono).text_color(hx.red).child(format!("−{}", file.dels))),
            );

            // Only the narrow layout nests the body under its row; two-pane
            // draws it in the column beside the list.
            if expanded && !two_pane {
                match self.md_preview.as_ref().filter(|(f, _)| f == &path) {
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
                    None => files = files.child(self.render_diff_body(hx, &path, cx)),
                }
            }
        }

        files.into_any_element()
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
    fn render_focused_diff(&self, hx: &HarnessTheme, cx: &mut Context<Self>) -> AnyElement {
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
        let body = match self.md_preview.as_ref().filter(|(f, _)| f == &file) {
            Some((_, md)) => div().size_full().child(md.clone()).into_any_element(),
            None => self.render_diff_body(hx, &file, cx),
        };
        div()
            .id("diff-focused")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_scroll()
            .p_1()
            .child(body)
            .into_any_element()
    }
}
