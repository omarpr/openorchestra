//! One file's diff, row by row: parsing and caching rows, and drawing them
//! unified or side by side.

use super::*;

impl HarnessWorkspace {
    /// Parsed rows for `file`, fetched on demand and cached for the life of the
    /// current diff. `None` means the fetch is still running.
    ///
    /// Two costs are being avoided, and they are different. *Parsing* is the
    /// expensive half of drawing a body — re-deriving 3 000 rows every frame
    /// spends milliseconds re-computing something that has not changed since
    /// the last `git diff` — which is what the cache is for. *Fetching* is the
    /// expensive half of opening a large branch: a changeset of tens of
    /// thousands of lines used to be read in full before the panel could draw
    /// anything, to show the one file on screen. Now each file's body is read
    /// when it is first looked at.
    ///
    /// The cache is emptied wholesale when a new diff lands, so an entry can
    /// never outlive the text it describes, and a fetch carries the `diff_gen`
    /// it started under so a slow one cannot populate the cache for a diff the
    /// user has already moved off. `RefCell` because rendering takes `&self` —
    /// the view is single-threaded and nothing re-enters this.
    pub(crate) fn parsed_rows(
        &self,
        file: &str,
        cx: &mut Context<Self>,
    ) -> Option<std::sync::Arc<Vec<DiffRow>>> {
        match self.row_cache.borrow().get(file) {
            Some(FileRows::Ready(rows)) => return Some(rows.clone()),
            Some(FileRows::Fetching) => return None,
            None => {}
        }
        let (worktree, base, record) = self.file_diff_source(file)?;
        // Claimed before the spawn, so the frames drawn while this runs find
        // `Fetching` and do not start a second one.
        self.row_cache
            .borrow_mut()
            .insert(file.to_string(), FileRows::Fetching);

        let generation = self.diff_gen;
        let key = file.to_string();
        cx.spawn(async move |view, cx| {
            let text = cx
                .background_spawn(async move {
                    crate::data::compute_file_diff(
                        &GitWorktrees::new(SystemRunner),
                        &worktree,
                        &base,
                        &record,
                    )
                })
                .await;
            let _ = view.update(cx, |ws, cx| {
                if ws.diff_gen != generation {
                    return;
                }
                let rows = std::sync::Arc::new(readable_diff_lines(&text));
                ws.row_cache.borrow_mut().insert(key, FileRows::Ready(rows));
                cx.notify();
            });
        })
        .detach();
        None
    }

    /// Rows for `file` only if they are already parsed.
    ///
    /// For the paths that act on a body the user is *looking at* — copying a
    /// selection, resolving an edit target — where the rows must already be
    /// cached because the interaction required them on screen. Never fetches:
    /// these are called from event handlers, where a miss means "no selection
    /// to act on", not "wait for git".
    pub(crate) fn cached_rows(&self, file: &str) -> Option<std::sync::Arc<Vec<DiffRow>>> {
        match self.row_cache.borrow().get(file) {
            Some(FileRows::Ready(rows)) => Some(rows.clone()),
            _ => None,
        }
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
    ///
    /// `body_width` is what split view is vetoed against — see `use_split`.
    pub(crate) fn render_diff_body(
        &self,
        hx: &HarnessTheme,
        file: &str,
        bounded: bool,
        body_width: f32,
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
                // A selection cannot exist without the rows being on screen,
                // so a miss here means there is nothing to target.
                let Some(rows) = ws.cached_rows(&file) else {
                    return;
                };
                let line = target_line_for_row(&rows, from);
                ws.request_edit(file, line, window, cx);
            }));

        let Some(rows) = self.parsed_rows(file, cx) else {
            // The fetch is in flight. Distinct from "—": this file has changes,
            // its text is simply not here yet.
            return wrapper
                .flex_none()
                .child(div().p_2().text_color(hx.tx2).child("loading diff…"))
                .into_any_element();
        };
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
        let split = use_split(self.prefs.diff_view, body_width);
        let mut items: Vec<BodyItem> = Vec::with_capacity(shown);
        // A note hangs off whichever row the line belongs to, which in split
        // view is the pair's anchor — the same row a click on that line acts on,
        // so a comment appears under the line that opened it.
        let push_note = |items: &mut Vec<BodyItem>, ix: usize| {
            if let (Some(k), Some(n)) = (worktree_key.as_deref(), rows[ix].number)
                && self.review.get(k, file, n).is_some()
            {
                items.push(BodyItem::Note(n));
            }
        };
        if split {
            for sr in split_rows(&rows[..shown]) {
                items.push(BodyItem::Split(sr));
                if let Some(ix) = sr.anchor() {
                    push_note(&mut items, ix);
                }
            }
        } else {
            for ix in 0..shown {
                items.push(BodyItem::Row(ix));
                push_note(&mut items, ix);
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
        // Unified: long lines may exceed the list's width instead of being
        // squeezed into it, and the container scrolls sideways to reach them.
        //
        // Split: the opposite is required. Two columns are only halves of
        // something if that something is the list's own width — let rows size
        // to their content and each pair's halves land wherever the longer line
        // puts them, so the divider stops being a straight line down the body.
        // The cost is that a line wider than its column is clipped there.
        .with_horizontal_sizing_behavior(if split {
            gpui::ListHorizontalSizingBehavior::FitList
        } else {
            gpui::ListHorizontalSizingBehavior::Unconstrained
        });

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
        let wrapper = wrapper.when(crate::probe_paint(), |d| {
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
    pub(crate) fn render_body_item(
        &self,
        hx: &HarnessTheme,
        file: &str,
        rows: &std::sync::Arc<Vec<DiffRow>>,
        item: BodyItem,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let transparent = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
        let ix = match item {
            BodyItem::Split(sr) => return self.render_split_item(hx, file, rows, sr, cx),
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

    /// One side-by-side line: old on the left, new on the right.
    ///
    /// Drawn at `DIFF_ROW_H` like every other body item, because `uniform_list`
    /// measures the first one and assumes the rest match — the same contract
    /// hunk rules and review notes are held to.
    ///
    /// A file header or hunk rule spans both columns, so it delegates straight
    /// back to the unified renderer rather than being drawn twice.
    pub(crate) fn render_split_item(
        &self,
        hx: &HarnessTheme,
        file: &str,
        rows: &std::sync::Arc<Vec<DiffRow>>,
        sr: SplitRow,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (left, right) = match sr {
            SplitRow::Full(ix) => {
                return self.render_body_item(hx, file, rows, BodyItem::Row(ix), cx);
            }
            SplitRow::Pair { left, right } => (left, right),
        };

        // Both halves select the same underlying row, so clicking either side
        // of a modification highlights the whole line — the pair is one line as
        // far as review is concerned.
        let anchor = sr.anchor();
        let selected = anchor.is_some_and(|ix| {
            self.diff_selection
                .as_ref()
                .is_some_and(|(f, from, to)| f == file && ix >= *from && ix <= *to)
        });

        // `w_full` is load-bearing: the halves are sized as shares of this row,
        // and a row with no width of its own is content-sized — so each line's
        // halves landed wherever that line's text put them and the divider
        // zig-zagged down the body, with insert-only lines drawn from the left
        // edge as if they were old text.
        let mut row = div()
            .id(("splitrow", anchor.unwrap_or(usize::MAX)))
            .flex()
            .w_full()
            .h(px(DIFF_ROW_H));
        if let Some(ix) = anchor {
            let row_file = file.to_string();
            let move_file = file.to_string();
            let dbl_file = file.to_string();
            let dbl_rows = rows.clone();
            row = row
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, ev: &MouseDownEvent, window, cx| {
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
                .on_mouse_move(cx.listener(move |ws, _: &MouseMoveEvent, _, cx| {
                    if ws.diff_selecting {
                        ws.extend_diff_selection(&move_file, ix, cx);
                    }
                }));
        }

        row.child(self.render_split_half(hx, file, rows, left, false, selected, cx))
            .child(div().flex_none().w(px(1.0)).h_full().bg(hx.border))
            .child(self.render_split_half(hx, file, rows, right, true, selected, cx))
            .into_any_element()
    }

    /// One column of a split line. `None` is the empty cell opposite a pure
    /// insertion or deletion — tinted, so an unbalanced hunk reads as "nothing
    /// was here" rather than as a blank the renderer forgot to fill.
    ///
    /// Exactly half the body: two `flex_1` columns with a zero basis share the
    /// full-width row equally whatever their text, and `overflow_hidden` clips a
    /// line wider than its column. `min_w_0` lets a long line shrink its column
    /// back to its share instead of claiming its content width.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render_split_half(
        &self,
        hx: &HarnessTheme,
        file: &str,
        rows: &std::sync::Arc<Vec<DiffRow>>,
        ix: Option<usize>,
        is_new: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let transparent = Rgba { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
        let half = div().flex().flex_1().flex_basis(px(0.0)).min_w_0().h_full().overflow_hidden();

        let Some(ix) = ix else {
            return half.bg(tint(hx.tx2, 0.06)).into_any_element();
        };
        let r = &rows[ix];
        let (fg, bg, marker) = match r.kind {
            DiffLineKind::Added => (rgb(DIFF_ADD_TX), tint(hx.green, 0.13), "+"),
            DiffLineKind::Removed => (rgb(DIFF_DEL_TX), tint(hx.red, 0.13), "−"),
            _ => (hx.tx2, transparent, " "),
        };
        let bg = if selected { tint(hx.accent, 0.22) } else { bg };

        let line_no = r.number;
        let comment_file = file.to_string();
        // Comments belong to the working file, so only the new side offers the
        // affordance: a removed line's number addresses a line that is gone,
        // and a comment pinned to it could not be shown against anything.
        let commentable = is_new && line_no.is_some() && r.kind != DiffLineKind::Removed;
        let has_comment = commentable
            && line_no.is_some_and(|n| {
                self.resolve()
                    .is_some_and(|res| self.review.get(&res.key, file, n).is_some())
            });

        let mut gutter = div()
            .id(("splitgutter", ix * 2 + usize::from(is_new)))
            .flex_none()
            .w(px(46.0))
            .pr_1()
            .flex()
            .items_center()
            .gap_1()
            .when(has_comment, |d| d.bg(tint(hx.amber, 0.18)))
            .text_color(if has_comment { hx.amber } else { hx.tx2 })
            .child(div().flex_none().w(px(8.0)).child(if has_comment { "▸" } else { "" }))
            .child(
                div()
                    .flex_1()
                    .child(line_no.map(|n| n.to_string()).unwrap_or_default()),
            );
        if commentable {
            gutter = gutter
                .cursor_pointer()
                .tooltip(tip(if has_comment { "Edit comment" } else { "Comment on line" }))
                .hover(|d| d.bg(hx.surface).text_color(hx.accent))
                .on_click(cx.listener(move |ws, ev: &gpui::ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if let Some(line) = line_no {
                        ws.begin_comment(comment_file.clone(), line, ev.position(), window, cx);
                    }
                }));
        }

        half.bg(bg)
            .child(gutter)
            .child(div().flex_none().w(px(10.0)).text_color(fg).child(marker))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pr_2()
                    .text_color(fg)
                    .whitespace_nowrap()
                    .child(r.text.clone()),
            )
            .into_any_element()
    }
}
