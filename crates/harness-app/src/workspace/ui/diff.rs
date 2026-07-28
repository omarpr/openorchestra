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

    /// Parsed rows for `file`, cached for the life of the current diff.
    ///
    /// Parsing is the expensive half of drawing a body: re-deriving 3 000 rows
    /// on every frame spends milliseconds re-computing something that has not
    /// changed since the last `git diff`. The cache is keyed by file and emptied
    /// wholesale when a new diff lands, so an entry can never outlive the text it
    /// describes. `RefCell` because rendering takes `&self` — the view is
    /// single-threaded and nothing re-enters this.
    pub(crate) fn parsed_rows(&self, file: &str) -> std::sync::Arc<Vec<DiffRow>> {
        if let Some(hit) = self.row_cache.borrow().get(file) {
            return hit.clone();
        }
        let rows = std::sync::Arc::new(readable_diff_lines(&self.unified_for_file(file)));
        self.row_cache
            .borrow_mut()
            .insert(file.to_string(), rows.clone());
        rows
    }

    /// The unified diff body, rendered inline beneath the file it belongs
    /// to (or beneath the whole list when no single file is selected).
    ///
    /// Virtualized. Every row used to be built on every frame: 3 000 rows cost
    /// 21 ms to *build* in debug and 4–7 ms in release, before gpui laid out or
    /// painted some twelve thousand nodes, each with a hitbox and listeners.
    /// The frame budget is 16 ms, so a big file could not be scrolled — each
    /// scroll frame rebuilt the entire file. A `uniform_list` builds only the
    /// rows on screen, which is why the cap below can now be large.
    ///
    /// `bounded` is the narrow layout, where a body is expanded *inside* the
    /// file list's own scroll area and so needs a definite height; the two-pane
    /// layout gives it the whole right column.
    pub(crate) fn render_diff_body(
        &self,
        hx: &HarnessTheme,
        file: &str,
        bounded: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _started = std::time::Instant::now();
        let wrapper = div()
            .id("diff-preview")
            .flex()
            .flex_col()
            .my_1()
            .rounded_md()
            .bg(hx.bg0)
            .border_1()
            .border_color(hx.border)
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
                let rows = ws.parsed_rows(&file);
                let line = target_line_for_row(&rows, from);
                ws.request_edit(file, line, window, cx);
            }));

        let rows = self.parsed_rows(file);
        if rows.is_empty() {
            return wrapper
                .flex_none()
                .child(div().p_2().text_color(hx.tx2).child("—"))
                .into_any_element();
        }

        // Flattened fresh each frame, unlike the parse: a note appears the
        // moment a comment is saved, while the diff behind it has not changed.
        // Walking the parsed rows to interleave notes is tens of microseconds —
        // it is *building elements* that had to be bounded, not iterating.
        let worktree_key = self.resolve().map(|r| r.key);
        let shown = rows.len().min(MAX_DIFF_LINES);
        let mut items: Vec<BodyItem> = Vec::with_capacity(shown);
        for ix in 0..shown {
            items.push(BodyItem::Row(ix));
            if let (Some(k), Some(n)) = (worktree_key.as_deref(), rows[ix].number)
                && self.review.get(k, file, n).is_some()
            {
                items.push(BodyItem::Note(n));
            }
        }
        if rows.len() > shown {
            items.push(BodyItem::Truncated(rows.len() - shown));
        }

        let count = items.len();
        let items = std::sync::Arc::new(items);
        let rows_for_items = rows.clone();
        let file_owned = file.to_string();
        let theme_for_items = hx.clone();

        let list = gpui::uniform_list(
            // Unique per file: the narrow layout can have several bodies
            // expanded at once, and a shared id would make them share a scroll
            // position.
            gpui::ElementId::Name(format!("diff-rows:{file}").into()),
            count,
            cx.processor(move |ws, range: std::ops::Range<usize>, _window, cx| {
                range
                    .map(|i| {
                        ws.render_body_item(
                            &theme_for_items,
                            &file_owned,
                            &rows_for_items,
                            items[i],
                            cx,
                        )
                    })
                    .collect()
            }),
        )
        // `Infer` — the height comes from item count, clamped to the space
        // available. The default is `Auto`, documented as "should not calculate
        // a fixed size", which resolves against the parent instead: give it an
        // ancestor chain that is content-sized and the list asks its content how
        // tall to be, its content is one measured item, and the whole diff
        // collapses to a 15 px strip in an empty pane. `Infer` is also what
        // zed's own project panel uses, for the same reason.
        .with_sizing_behavior(gpui::ListSizingBehavior::Infer)
        // Long lines may exceed the list's width instead of being squeezed into
        // it; the container scrolls sideways to reach them.
        .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained);

        // 15 px is `DIFF_ROW_H`, the height every item is drawn at — uniform_list
        // measures the first item and assumes the rest match, which is exactly
        // why notes and hunk rules are drawn at the row height too.
        let list = if bounded {
            list.h(px((count as f32 * DIFF_ROW_H).min(INLINE_BODY_MAX_H)))
        } else {
            list.flex_grow_1().min_h_0()
        };
        log::trace!("diff body: {count} items prepared in {:?}", _started.elapsed());
        // Same idiom as the settings overlay: a virtualized list that lays out
        // to zero height reports perfect state and paints nothing, which is
        // indistinguishable from "the diff is empty" without this.
        let wrapper = wrapper.when(std::env::var_os("HARNESS_PROBE_PAINT").is_some(), |d| {
            d.child(
                gpui::canvas(
                    |b, _, _| log::info!("paint: diff body bounds = {b:?}"),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
        });
        wrapper
            .when(!bounded, |d| d.flex_1().h_full().min_h_0())
            .child(list)
            .into_any_element()
    }

    /// One 15 px line of the body: a diff row, a review note pinned under the
    /// line it annotates, or the "this file was truncated" tail.
    fn render_body_item(
        &self,
        hx: &HarnessTheme,
        file: &str,
        rows: &std::sync::Arc<Vec<DiffRow>>,
        item: BodyItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let transparent = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
        let ix = match item {
            BodyItem::Truncated(n) => {
                // Never truncate silently — a diff that just stops looks like
                // the change is smaller than it is.
                return div()
                    .h(px(DIFF_ROW_H))
                    .px_2()
                    .text_color(hx.amber)
                    .whitespace_nowrap()
                    .child(format!("… {n} more lines — view the full diff in git"))
                    .into_any_element();
            }
            BodyItem::Note(line) => {
                let body = self
                    .resolve()
                    .and_then(|r| self.review.get(&r.key, file, line).map(|c| c.body.clone()))
                    .unwrap_or_default();
                // One line, ellipsized: the note is a marker that a comment is
                // here, and the comment editor holds the full text.
                return div()
                    .flex()
                    .h(px(DIFF_ROW_H))
                    .items_center()
                    .pl(px(56.0))
                    .pr_2()
                    .bg(tint(hx.amber, 0.10))
                    .border_l_2()
                    .border_color(hx.amber)
                    .text_color(hx.tx1)
                    .overflow_hidden()
                    .child(div().min_w_0().whitespace_nowrap().child(body))
                    .into_any_element();
            }
            BodyItem::Row(ix) => ix,
        };
        let r = &rows[ix];

        // A hunk boundary is a thin rule plus git's section text (the enclosing
        // function or heading). The old `@@ -a,b +c,d @@` coordinates are gone:
        // the gutter shows the real line numbers, which is the same information
        // without the decoding step.
        if r.kind == DiffLineKind::HunkBreak {
            return div()
                .flex()
                .items_center()
                .gap_2()
                .h(px(DIFF_ROW_H))
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
                )
                .into_any_element();
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
            return div()
                .flex()
                .h(px(DIFF_ROW_H))
                .bg(row_bg)
                .px_2()
                .text_color(fg)
                .whitespace_nowrap()
                .child(r.text.clone())
                .into_any_element();
        }

        let line_no = r.number;
        let comment_file = file.to_string();
        let row_file = file.to_string();
        let move_file = file.to_string();
        let dbl_file = file.to_string();
        let dbl_rows = rows.clone();
        let has_comment = line_no.is_some_and(|n| {
            self.resolve()
                .is_some_and(|r| self.review.get(&r.key, file, n).is_some())
        });

        div()
            .id(("diffrow", ix))
            .flex()
            .h(px(DIFF_ROW_H))
            .bg(row_bg)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, ev: &MouseDownEvent, window, cx| {
                    // A second click on a diff line means "let me change this".
                    // The diff itself cannot be typed into, so offer the file
                    // that can.
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
            // Dragging over rows extends the selection, which is what "highlight
            // some lines" means with a mouse.
            .on_mouse_move(cx.listener(move |ws, _: &MouseMoveEvent, _, cx| {
                if ws.diff_selecting {
                    ws.extend_diff_selection(&move_file, ix, cx);
                }
            }))
            .child(
                // Fixed-width gutter so the code column stays aligned as numbers
                // grow past 999. Doubles as the comment affordance.
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
                    // The marker sits in a fixed slot rather than being prefixed
                    // to the number, so commented lines do not shift their digits
                    // sideways.
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
                    .on_click(cx.listener(move |ws, ev: &gpui::ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(line) = line_no {
                            ws.begin_comment(
                                comment_file.clone(),
                                line,
                                ev.position(),
                                window,
                                cx,
                            );
                        }
                    })),
            )
            .child(div().flex_none().w(px(10.0)).text_color(fg).child(marker))
            .child(
                div()
                    .flex_none()
                    .pr_2()
                    .text_color(fg)
                    .whitespace_nowrap()
                    .child(r.text.clone()),
            )
            .into_any_element()
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
            // `h_full` beside `flex_1`, the pairing workspace.rs already
            // documents: flex_1 alone stretches the box but leaves the height
            // *indefinite*, so everything below resolves against content
            // instead. A virtualized list asked to size itself against its own
            // content is the degenerate case — see `render_diff_body`.
            .h_full()
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
                    .h_full()
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

    /// The diff-base button: what the panel is measuring against, and the
    /// handle for changing it.
    fn render_diff_base_button(
        &self,
        hx: &HarnessTheme,
        r: &Resolved,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = self.prefs.diff_base.label(&r.compare_ref);
        div()
            .id("diff-base")
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .h(px(20.0))
            .px_1p5()
            .rounded_md()
            .bg(hx.surface)
            .text_xs()
            .font_family(hx.font_mono)
            .text_color(hx.tx1)
            .cursor_pointer()
            .tooltip(tip("What the diff is measured against"))
            .hover(|d| d.bg(hx.surface_hover).text_color(hx.tx0))
            .child(label)
            .child(div().text_color(hx.tx2).child("▾"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, ev: &MouseDownEvent, _, cx| {
                    // Anchored at the click, like the tab strip's `+` menu. Down
                    // rather than click so the backdrop's own mouse-down (which
                    // closes it) cannot swallow the toggle.
                    cx.stop_propagation();
                    ws.diff_base_menu = match ws.diff_base_menu {
                        Some(_) => None,
                        None => Some(ev.position),
                    };
                    cx.notify();
                }),
            )
    }

    /// The two-row diff-base menu, anchored under its button.
    pub(crate) fn render_diff_base_menu(
        &self,
        hx: &HarnessTheme,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let compare = self
            .resolve()
            .map(|r| r.compare_ref)
            .unwrap_or_else(|| "HEAD".to_string());
        let active = self.prefs.diff_base;
        let row = |base: DiffBase, hint: &'static str, cx: &mut Context<Self>| {
            let selected = base == active;
            div()
                .id(gpui::SharedString::from(format!("diff-base-{}", base.label(&compare))))
                .flex()
                .flex_col()
                .px_2()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .hover(|d| d.bg(hx.surface_hover))
                .on_click(cx.listener(move |ws, _, _, cx| {
                    cx.stop_propagation();
                    ws.set_diff_base(base, cx);
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_sm()
                        .text_color(if selected { hx.tx0 } else { hx.tx1 })
                        .child(div().w(px(10.0)).flex_none().text_xs().text_color(hx.accent).child(
                            if selected { "✓" } else { " " },
                        ))
                        .child(base.label(&compare)),
                )
                .child(
                    div()
                        .pl(px(14.0))
                        .text_xs()
                        .text_color(hx.tx2)
                        .child(hint),
                )
        };

        div()
            .absolute()
            .left(at.x)
            .top(at.y + px(12.0))
            .w(px(230.0))
            .p(px(4.0))
            .rounded_md()
            .bg(hx.surface2)
            .border_1()
            .border_color(hx.border2)
            .flex()
            .flex_col()
            .gap_1()
            // Same reason as the card menu: the dismiss backdrop listens on
            // mouse-down, and a click is down-then-up.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _, cx| cx.stop_propagation()),
            )
            .child(row(DiffBase::Uncommitted, "changes you have not committed", cx))
            .child(row(DiffBase::DefaultBranch, "everything this branch changes", cx))
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
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            // The base sits before the count, because it is what
                            // the count *means*: "3 changed files" answers
                            // nothing until you know 3 since when.
                            .child(self.render_diff_base_button(hx, r, cx))
                            .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).child(
                                if self.diff_pending {
                                    "Changed files  …".to_string()
                                } else {
                                    let total = self.diff.files.len();
                                    let done = self.review.reviewed_count(
                                        &r.key,
                                        self.diff.files.iter().map(|f| f.path.as_str()),
                                    );
                                    let elided = self.diff.untracked_elided;
                                    match (done, elided) {
                                        // Never imply the list is complete when
                                        // it is not.
                                        (0, 0) => format!("Changed files  {total}"),
                                        (0, n) => format!("Changed files  {total}  (+{n} untracked not shown)"),
                                        (d, 0) => format!("Changed files  {d}/{total} reviewed"),
                                        (d, n) => format!("Changed files  {d}/{total} reviewed  (+{n} untracked not shown)"),
                                    }
                                },
                            )),
                    )
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
                    cx,
                );
            }
        }

        files.into_any_element()
    }

    /// Append one changed-file row (and, in accordion mode, its body) to the
    /// file list. `key` is the stable identity; `display_path` is what the row
    /// shows (relative under a project group for unified diffs).
    fn append_diff_file_row(
        &self,
        mut files: gpui::Stateful<gpui::Div>,
        hx: &HarnessTheme,
        i: usize,
        key: &str,
        display_path: &str,
        file: &worktrees::DiffFile,
        worktree_key: Option<&str>,
        two_pane: bool,
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
                .child(
                    div()
                        .id(("reviewed", i))
                        .flex_none()
                        .w(px(14.0))
                        .text_xs()
                        .font_family(hx.font_mono)
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
                            ::ui::Icon::new(::ui::IconName::File)
                                .size(::ui::IconSize::Indicator),
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
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis_middle()
                        .child(display_path.to_string()),
                )
                .when(open && crate::markdown_preview::is_markdown(&md_check), |d| {
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
                None => files = files.child(self.render_diff_body(hx, &body_key, true, cx)),
            }
        }
        files
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
        let container =
            div().id("diff-focused").flex().flex_col().flex_1().h_full().min_w_0().min_h_0().p_1();
        match self.md_preview.as_ref().filter(|(f, _)| f == &file) {
            Some((_, md)) => container
                .overflow_scroll()
                .child(div().size_full().child(md.clone()))
                .into_any_element(),
            // Horizontal only: long lines still scroll sideways, while the
            // vertical axis stays hidden so the list keeps its definite height.
            None => container
                .overflow_x_scroll()
                .child(self.render_diff_body(hx, &file, false, cx))
                .into_any_element(),
        }
    }
}
