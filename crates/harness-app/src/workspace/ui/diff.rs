//! Split out of `workspace.rs` (see its module docs). Pure relocation:
//! `impl HarnessWorkspace` methods for this feature area.

use super::super::*;

mod body;
mod header;
mod files;

impl HarnessWorkspace {

    /// Where one file's diff has to be fetched from: its worktree, the base the
    /// summary pinned, and the file record naming it to git.
    ///
    /// `key` is the panel's identity key: a worktree-relative path, or
    /// `repo/rel` for a unified task diff. Git only knows `rel`, so a unified
    /// key resolves back through the member that contributed it — which is also
    /// what supplies that member's own worktree and base.
    fn file_diff_source(&self, key: &str) -> Option<(PathBuf, String, worktrees::DiffFile)> {
        if let Some(f) = self
            .unified_diff
            .files
            .iter()
            .find(|f| format!("{}/{}", f.repo, f.file.path) == key)
        {
            return Some((f.worktree.clone(), f.base.clone(), f.file.clone()));
        }
        // The flattened list keys by the same `key`, so its record already
        // names the file the way git does in the single-worktree case.
        let file = self.diff.files.iter().find(|f| f.path == key)?.clone();
        let r = self.resolve()?;
        Some((r.path, self.diff.base.clone(), file))
    }

    /// The review toolbar: how many comments there are, and what can be done
    /// with them.
    ///
    /// Shown only when there is something to act on. Adding a comment is a
    /// click on a line's gutter number in the diff itself, so this bar is
    /// purely about what to do with the comments you already have.
    ///
    /// In the tools panel tab strip the trailing chips sit after a `flex_1`
    /// spacer already owned by the strip; skip the internal spacers so the
    /// chips pack tightly on the right.
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

        let in_tool_strip = self.prefs.diff_location == DiffLocation::Dock
            && self.prefs.tool_tab == ToolTab::Review;

        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .font_family(hx.font_mono);
        if !in_tool_strip {
            row = row.px_3().pb_1().child(div().flex_1()).child(div().flex_1());
        }
        row.child(
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
                if self.clearing_review {
                    "discard?".to_string()
                } else {
                    "clear".to_string()
                },
                if self.clearing_review { hx.red } else { hx.tx2 },
            )
            .on_click(cx.listener(|ws, _, _, cx| ws.clear_review(cx))),
        )
    }

    /// The diff review surface, drawn identically in the tools panel and in a tab.
    ///
    /// `dock_width` is `Some` for the tools panel and `None` for a tab — the only
    /// thing the two differ by, since a tab always has room for two columns.
    ///
    /// `show_review_bar` is false when the tools panel already shows review
    /// actions in its tab strip.
    pub(crate) fn render_diff_surface(
        &self,
        hx: &HarnessTheme,
        dock_width: Option<f32>,
        show_review_bar: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(r) = self.resolve() else {
            return self.render_no_project(hx).into_any_element();
        };
        let two_pane = use_two_pane(
            self.prefs.diff_location,
            dock_width.unwrap_or(f32::INFINITY),
            self.prefs.diff_view,
            self.prefs.diff_list_width,
        );
        let (adds, dels) = self.diff.totals();
        let review_count = self.review.count(&r.key);
        // What the body actually gets, which is what split view is vetoed
        // against. Decided from the persisted dock width for the same reason
        // `use_two_pane` is: a tab always fills the center, so it is treated as
        // unbounded rather than requiring a layout pass to measure.
        let surface_w = dock_width.unwrap_or(f32::INFINITY);
        let body_w = if two_pane {
            surface_w - self.prefs.diff_list_width - SPLITTER_W
        } else {
            surface_w
        };

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
            .child(self.render_diff_header(hx, &r, adds, dels, body_w, cx));
        if show_review_bar && review_count > 0 {
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
                    .child(self.render_file_list(hx, true, body_w, cx))
                    .child(self.splitter(hx, Drag::DiffList, cx))
                    .child(self.render_focused_diff(hx, body_w, cx)),
            )
        } else {
            panel.child(self.render_file_list(hx, false, body_w, cx))
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

}
