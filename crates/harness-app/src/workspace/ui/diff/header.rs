//! The diff panel's header controls: compare-base picker, unified/split
//! toggle, and the push button.

use super::*;

impl HarnessWorkspace {
    /// The branch line and the changed-files/±counts line above the review.
    /// The push control, shown only when this branch actually has commits the
    /// remote does not.
    ///
    /// Two steps, like the review bar's clear/discard: the armed label names the
    /// remote and the count, because a button reading "push" alone does not say
    /// where the work is about to go — and for a branch with no upstream, where
    /// it goes is a remote branch that does not exist yet.
    pub(crate) fn render_push_control(
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
                // Armed, this reads `push 3 → origin/<branch>?` — ~345px for a
                // real branch name, against ~196px of header at the panel's
                // minimum width. It is the one control here that is unbounded,
                // so it is the one that gives ground.
                .min_w_0()
                .truncate()
                .items_center()
                .h(px(20.0))
                .px_1p5()
                .rounded_md()
                .text_xs()
                .font_family(hx.font_ui)
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
    pub(crate) fn render_diff_base_button(
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
            .font_family(hx.font_ui)
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

    /// The unified/split toggle.
    ///
    /// `available` is the width the body will get: below the split threshold
    /// the split half is drawn inactive and says why, rather than silently
    /// accepting a click that changes nothing on screen.
    pub(crate) fn render_diff_view_toggle(
        &self,
        hx: &HarnessTheme,
        available: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let current = self.prefs.diff_view;
        let too_narrow = available < SPLIT_MIN_W;
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_0p5()
            .p(px(1.0))
            .rounded_md()
            .bg(hx.surface);
        for view in [DiffView::Unified, DiffView::Split] {
            let selected = current == view;
            // Split is vetoed by width, not un-picked by it: the pref stays,
            // so widening the dock restores the layout the user asked for.
            let vetoed = view == DiffView::Split && too_narrow;
            let tooltip = if vetoed {
                "Too narrow to split — click to widen the panel"
            } else {
                match view {
                    DiffView::Unified => "One column, with +/− markers",
                    DiffView::Split => "Old and new side by side",
                }
            };
            row = row.child(
                div()
                    .id(gpui::SharedString::from(format!("diff-view-{}", view.label())))
                    .flex()
                    .items_center()
                    .h(px(18.0))
                    .px_1p5()
                    .rounded_md()
                    .text_xs()
                    .when(selected, |d| d.bg(hx.surface2).text_color(hx.tx0))
                    .when(!selected, |d| {
                        d.text_color(if vetoed { hx.tx2 } else { hx.tx1 })
                            .hover(|h| h.bg(hx.surface_hover).text_color(hx.tx0))
                    })
                    .cursor_pointer()
                    .tooltip(tip(tooltip))
                    .child(view.label())
                    .on_click(cx.listener(move |ws, _, window, cx| {
                        cx.stop_propagation();
                        ws.set_diff_view(view, cx);
                        if view == DiffView::Split && ws.prefs.diff_location == DiffLocation::Dock {
                            let ceiling = max_diff_width(
                                f32::from(window.viewport_size().width),
                                ws.prefs.sidebar_width,
                                ws.prefs.sidebar_open,
                            );
                            if ws.tools_maximized {
                                // Already as wide as the window allows.
                            } else if let Some(w) = split_dock_width(ws.prefs.diff_width, ceiling) {
                                ws.prefs.diff_width = w;
                                save_prefs_to(&ws.prefs_path, &ws.prefs);
                                cx.notify();
                            } else if ws.prefs.diff_width < SPLIT_MIN_W {
                                // Even the widest drag can't fit two columns
                                // beside the center: give the panel the center's
                                // space instead, so Split is never a dead toggle.
                                ws.tools_maximized = true;
                                cx.notify();
                            }
                        }
                    })),
            );
        }
        row
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

    pub(crate) fn render_diff_header(
        &self,
        hx: &HarnessTheme,
        r: &Resolved,
        adds: usize,
        dels: usize,
        body_width: f32,
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
                    .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx2).child(
                        if self.prefs.diff_location == DiffLocation::Dock {
                            "REVIEW"
                        } else {
                            "DIFF"
                        },
                    ))
                    // `truncate()` (= overflow_hidden + whitespace_nowrap +
                    // text_ellipsis), not bare `overflow_hidden()`: clipping
                    // stops a box painting outside itself but does nothing about
                    // *wrapping*, and a long branch wrapped to three lines inside
                    // this `h(px(34.0))` row overflowed it above and below —
                    // painting over the row beneath.
                    .child(div().flex_1().min_w_0().text_xs().font_family(hx.font_ui).text_color(hx.tx1).truncate().child(r.branch.clone()))
                    .child(self.review_overview_button(hx, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    // Below ~400px the two groups cannot share a line: the left
                    // wants ~207px and the right ~160px, against 196px of usable
                    // width. Wrapping stacks them instead of letting them paint
                    // over each other. `space-between` with one item on a line
                    // degenerates to `flex-start`, so both lines left-align with
                    // no override needed.
                    .flex_wrap()
                    // Hard backstop. Wrapping and shrinking between them should
                    // keep everything inside, but the failure mode being fixed
                    // here is content painting over its neighbours, and clipping
                    // is a strictly better way to lose than that. The base menu
                    // is unaffected: it renders at the workspace root
                    // (`workspace.rs:1748`), not inside this row.
                    .overflow_hidden()
                    .items_center()
                    .justify_between()
                    .gap_x_2()
                    .gap_y_1()
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
                            .child(div().text_xs().font_weight(gpui::FontWeight::SEMIBOLD).text_color(hx.tx0).truncate().child(
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
                            // `min_w_0`, not `flex_none`: the group must take a
                            // whole line once wrapped, but it must still be able
                            // to shrink onto it. Pinned unshrinkable, the armed
                            // push control would hang off the panel's right edge
                            // — the same overflow this row exists to fix.
                            // Shrinkage is absorbed by the push label alone; the
                            // toggle (`flex_none` internally) and the counts hold
                            // their size, because a squeezed toggle is
                            // unclickable and a clipped count is a wrong number.
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .font_family(hx.font_ui)
                            .children(self.render_push_control(hx, r, cx))
                            .child(self.render_diff_view_toggle(hx, body_width, cx))
                            .child(div().flex_none().text_color(hx.green).child(format!("+{adds}")))
                            .child(div().flex_none().text_color(hx.red).child(format!("−{dels}"))),
                    ),
            )
    }
}
