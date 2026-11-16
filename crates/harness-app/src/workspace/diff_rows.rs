//! Turning unified diff text into display rows: line numbers, hunk headers,
//! side-by-side pairing, and the width rules for the diff panel.

use super::*;

/// Strip git's per-file metadata preamble from a unified diff.
///
/// `diff --git`, `index <sha>..<sha>`, mode/rename lines, and the `---` / `+++`
/// pair carry nothing a reader needs: the file being viewed is already named in
/// the changed-files list above, and in the all-files view we render our own
/// header. Left in, they cost five lines of noise at the top of every file —
/// which on a short change is most of what you can see.
///
/// `@@` hunk headers are kept: they carry the line numbers, which is real
/// navigational information.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum DiffLineKind {
    /// A synthesized per-file header, replacing git's `diff --git` preamble.
    FileHeader,
    /// A hunk boundary. Carries the enclosing section (the text git puts
    /// *after* the second `@@`) rather than the coordinates.
    HunkBreak,
    Added,
    Removed,
    Context,
}

/// One rendered row of the diff.
#[derive(Debug, PartialEq, Clone)]
pub struct DiffRow {
    pub kind: DiffLineKind,
    /// Line number for the gutter — the file's real line number, which is what
    /// the `@@ -a,b +c,d @@` coordinates encoded far less directly. Removed
    /// lines carry the *old* file's number, everything else the new file's.
    pub number: Option<u32>,
    pub text: String,
}

/// Parse a `@@ -old,n +new,n @@ section` header.
///
/// Returns the starting line numbers plus the trailing section text, which git
/// fills with the enclosing function or heading. The coordinates become the
/// gutter; the section is the only part worth showing as text.
pub(crate) fn parse_hunk(line: &str) -> Option<(u32, u32, String)> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = rest.split_once(" @@")?;
    let (old, new) = ranges.split_once(' ')?;
    let start = |spec: &str| -> Option<u32> {
        spec.trim_start_matches(['-', '+'])
            .split(',')
            .next()?
            .parse()
            .ok()
    };
    Some((start(old)?, start(new)?, section.trim().to_string()))
}

pub fn readable_diff_lines(unified: &str) -> Vec<DiffRow> {
    const NOISE: [&str; 7] = [
        "index ",
        "new file mode ",
        "deleted file mode ",
        "old mode ",
        "new mode ",
        "similarity index ",
        "dissimilarity index ",
    ];
    let row = |kind, number, text: String| DiffRow { kind, number, text };

    let mut out: Vec<DiffRow> = Vec::new();
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut first_hunk_of_file = true;

    for line in unified.lines() {
        // The `diff --git a/X b/Y` line is where a file starts. We drop the
        // command itself but keep the boundary, as a header — without it the
        // all-files view would run every file together.
        if let Some(rest) = line.strip_prefix("diff --git ") {
            // `diff --git a/<old> b/<new>` — take the new path, which is the
            // one that matters for a rename.
            let path = rest.rsplit_once(" b/").map(|(_, new)| new).unwrap_or(rest);
            out.push(row(DiffLineKind::FileHeader, None, path.to_string()));
            first_hunk_of_file = true;
            continue;
        }
        if NOISE.iter().any(|p| line.starts_with(p)) {
            continue;
        }
        // `--- a/x` and `+++ b/x` are the file pair, not content. Real content
        // lines are a single `-`/`+` followed by the text, so requiring the
        // triple form avoids eating a legitimate `--foo` deletion.
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename from ") {
            out.push(row(
                DiffLineKind::Context,
                None,
                format!("renamed from {rest}"),
            ));
            continue;
        }
        if let Some(rest) = line.strip_prefix("rename to ") {
            out.push(row(
                DiffLineKind::Context,
                None,
                format!("renamed to {rest}"),
            ));
            continue;
        }

        if let Some((old, new, section)) = parse_hunk(line) {
            old_no = old;
            new_no = new;
            // The first hunk right after a file header needs no separator —
            // the header already marks the boundary — unless git gave us a
            // section heading, which is worth showing on its own. Later hunks
            // always get one, because the line numbers jump.
            if !first_hunk_of_file || !section.is_empty() {
                out.push(row(DiffLineKind::HunkBreak, None, section));
            }
            first_hunk_of_file = false;
            continue;
        }

        // `\ No newline at end of file` is git commentary, not content.
        if line.starts_with('\\') {
            out.push(row(
                DiffLineKind::Context,
                None,
                line.trim_start_matches('\\').trim().to_string(),
            ));
            continue;
        }

        // Strip the leading +/-/space marker: the gutter and the row's tint
        // already say what happened, and keeping it shifts every line by one
        // column against the file's real indentation.
        let (kind, number, text) = if let Some(rest) = line.strip_prefix('+') {
            let n = new_no;
            new_no += 1;
            (DiffLineKind::Added, Some(n), rest)
        } else if let Some(rest) = line.strip_prefix('-') {
            let n = old_no;
            old_no += 1;
            (DiffLineKind::Removed, Some(n), rest)
        } else {
            let rest = line.strip_prefix(' ').unwrap_or(line);
            let n = new_no;
            new_no += 1;
            old_no += 1;
            (DiffLineKind::Context, Some(n), rest)
        };
        out.push(row(kind, number, text.to_string()));
    }
    out
}

/// Dock width at or above which the diff surface splits into two columns.
///
/// Below this a file list and a diff column would each be too narrow to read a
/// path or a line of code without truncation, so the accordion — which gives
/// the full width to whichever file is open — stays the better layout.
pub(crate) const DIFF_TWO_PANE_MIN: f32 = 560.0;

/// Absolute ceiling for a persisted panel width.
///
/// Not a layout constraint — the real one is computed against the live window
/// in `drag_move`. This only stops a corrupt prefs file, or one written on a
/// far larger display, from restoring a width that no longer means anything.
/// Wide enough for a 6K monitor, so it never binds in practice.
pub(crate) const MAX_PANEL_W: f32 = 6000.0;

/// Space the center pane keeps when the tools panel is dragged wide. Below
/// this the tab strip and terminal stop being usable, and the drag has to stop
/// rather than let the dock swallow the window. 240 rather than the old 360:
/// reviewing a split diff is exactly when you want the panel as wide as it
/// goes, and anyone who wants the center gone entirely maximizes the panel
/// (`tools_maximized`) instead.
pub(crate) const MIN_CENTER_W: f32 = 240.0;

/// The widest the diff dock may be dragged in a window this wide.
///
/// The dock is right-anchored, so dragging left widens it: without this the
/// drag runs until the center pane is gone. Floors at the dock's own minimum so
/// a very narrow window still yields a draggable (if cramped) dock rather than
/// an inverted range.
pub(crate) fn max_diff_width(viewport_w: f32, sidebar_w: f32, sidebar_open: bool) -> f32 {
    let taken = if sidebar_open { sidebar_w } else { 0.0 };
    (viewport_w - taken - MIN_CENTER_W).max(220.0)
}

/// Whether to draw the diff surface as files-left/changes-right.
///
/// Decided from the location and the persisted dock width rather than measured
/// bounds: a tab always fills the center, and the dock's width is a pref we
/// already own, so neither case needs a layout pass to answer.
///
/// With Split picked, the dock also has to leave the *body* wide enough to
/// split once the file list has taken its column. When it cannot, the accordion
/// wins: it gives the open file the whole dock, which is often exactly enough
/// for two columns. Before this, a dock between the two thresholds kept the
/// file list and so vetoed Split forever — picking Split in the tools panel
/// visibly did nothing on any laptop-sized window.
pub(crate) fn use_two_pane(location: DiffLocation, dock_width: f32, view: DiffView, list_width: f32) -> bool {
    match location {
        DiffLocation::Tab => true,
        DiffLocation::Dock => {
            dock_width >= DIFF_TWO_PANE_MIN
                && (view != DiffView::Split
                    || dock_width - list_width - SPLITTER_W >= SPLIT_MIN_W
                    || dock_width < SPLIT_MIN_W)
        }
    }
}

/// Width of the drag handle between the diff file list and the body (see
/// `splitter`).
pub(crate) const SPLITTER_W: f32 = 6.0;

/// The dock width to grow to when Split is picked in a dock too narrow for it,
/// or `None` when the current width already splits (or nothing would help).
///
/// Picking Split is a request to see two columns; a toggle that greys out and
/// does nothing reads as broken. So the click widens the dock to the narrowest
/// width that splits — in the accordion layout the body is the whole dock —
/// capped at what the window allows while keeping the center usable.
pub(crate) fn split_dock_width(dock_width: f32, ceiling: f32) -> Option<f32> {
    if dock_width >= SPLIT_MIN_W || ceiling < SPLIT_MIN_W {
        return None;
    }
    Some(SPLIT_MIN_W)
}

/// The line in the *working file* that diff row `ix` refers to.
///
/// Added and context rows already carry new-file numbers. A removed row does
/// not: `readable_diff_lines` gives it the OLD file's number, which addresses a
/// line that is gone. Opening the file there would land somewhere unrelated and
/// read as a targeting bug rather than as a missing line, so a removed row
/// resolves to the next row that still exists — stopping at the next file
/// header, since the following file's line 1 is not this deletion's neighbour.
/// A removed run at the end of a file has no successor and yields `None`: open
/// the file, don't guess.
pub(crate) fn target_line_for_row(rows: &[DiffRow], ix: usize) -> Option<u32> {
    let row = rows.get(ix)?;
    match row.kind {
        DiffLineKind::Added | DiffLineKind::Context => row.number,
        DiffLineKind::Removed => rows[ix + 1..]
            .iter()
            .take_while(|r| r.kind != DiffLineKind::FileHeader)
            .find(|r| matches!(r.kind, DiffLineKind::Added | DiffLineKind::Context))
            .and_then(|r| r.number),
        DiffLineKind::FileHeader | DiffLineKind::HunkBreak => None,
    }
}

/// Body width at or below which split view falls back to unified.
///
/// Two columns of roughly 240 px each — about 30 monospace characters of code
/// beside a line-number gutter. Below that neither side shows enough of a line
/// to compare against the other, and one readable column beats two unreadable
/// ones. It was 720 (zed's `minimum_split_diff_width`, which is sized for a
/// full editor pane); the tools panel tops out near 800 px on a laptop window
/// and also hosts the file list, so at 720 the dock could never split at all.
/// A split line wider than its column is clipped, by design (`FitList`).
pub(crate) const SPLIT_MIN_W: f32 = 480.0;

/// Whether to draw the body as old-and-new columns.
///
/// Split is a preference the width can veto, never the other way round: a
/// narrow dock renders unified without disturbing the pref, so widening the
/// dock brings split back rather than requiring the user to re-pick it.
pub(crate) fn use_split(view: DiffView, body_width: f32) -> bool {
    view == DiffView::Split && body_width >= SPLIT_MIN_W
}

/// One line of a split body: a row that spans both columns, or an old/new pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum SplitRow {
    /// A file header or hunk rule — structure, belonging to neither side.
    Full(usize),
    /// Old on the left, new on the right. At least one side is always `Some`;
    /// the other being `None` is what a pure insertion or deletion looks like.
    Pair {
        left: Option<usize>,
        right: Option<usize>,
    },
}

impl SplitRow {
    /// The row a click on this line acts on — selection, comments, edit intent.
    ///
    /// The new-file side wins when both exist, because every one of those
    /// actions is about the file as it stands now: a comment addresses the line
    /// you would edit, and `target_line_for_row` resolves against the working
    /// file. A deletion has no new side and falls back to the old one, which is
    /// exactly the case `target_line_for_row` already handles.
    pub(crate) fn anchor(self) -> Option<usize> {
        match self {
            SplitRow::Full(_) => None,
            SplitRow::Pair { left, right } => right.or(left),
        }
    }
}

/// Pair a unified row list into side-by-side lines.
///
/// Context occupies both columns at once. A run of removals is zipped against
/// the run of additions that immediately follows it — git emits a modification
/// in exactly that shape — and the shorter run pads with `None`, so a hunk that
/// replaces two lines with five leaves three empty cells on the left rather
/// than sliding the rest of the file out of alignment.
pub(crate) fn split_rows(rows: &[DiffRow]) -> Vec<SplitRow> {
    let mut out = Vec::with_capacity(rows.len());
    let mut i = 0;
    while i < rows.len() {
        match rows[i].kind {
            DiffLineKind::FileHeader | DiffLineKind::HunkBreak => {
                out.push(SplitRow::Full(i));
                i += 1;
            }
            DiffLineKind::Context => {
                out.push(SplitRow::Pair {
                    left: Some(i),
                    right: Some(i),
                });
                i += 1;
            }
            DiffLineKind::Removed => {
                let dels_start = i;
                while i < rows.len() && rows[i].kind == DiffLineKind::Removed {
                    i += 1;
                }
                let adds_start = i;
                while i < rows.len() && rows[i].kind == DiffLineKind::Added {
                    i += 1;
                }
                let (dels, adds) = (dels_start..adds_start, adds_start..i);
                for k in 0..dels.len().max(adds.len()) {
                    out.push(SplitRow::Pair {
                        left: (k < dels.len()).then(|| dels_start + k),
                        right: (k < adds.len()).then(|| adds_start + k),
                    });
                }
            }
            // Additions with no removals before them: a pure insertion, which
            // has nothing to sit opposite.
            DiffLineKind::Added => {
                out.push(SplitRow::Pair {
                    left: None,
                    right: Some(i),
                });
                i += 1;
            }
        }
    }
    out
}

/// Cap on diff lines held for one file.
///
/// This used to be a *rendering* cap of 5 000, because every line was its own
/// element on every frame and an enormous diff stalled the frame outright. The
/// body is virtualized now — only the rows on screen are built — so the cap
/// bounds parsing and memory rather than paint, and can sit far above any real
/// file. When it bites, the UI says so rather than truncating silently.
pub(crate) const MAX_DIFF_LINES: usize = 50_000;

/// Height every body item is drawn at.
///
/// `uniform_list` measures the first item and assumes the rest match, so this is
/// a contract, not a preference: hunk rules and review notes are drawn at the
/// row height because a taller one would desynchronise the list's arithmetic
/// from what is painted.
pub(crate) const DIFF_ROW_H: f32 = 15.0;

/// How tall an inline-expanded body may grow in the narrow layout, where it
/// lives inside the file list's own scroll area and so needs a definite height.
pub(crate) const INLINE_BODY_MAX_H: f32 = 420.0;

#[cfg(test)]
mod diff_render_tests {
    use super::{
        load_prefs, max_diff_width, move_task_on_disk, readable_diff_lines, reorder_base, reorder_panel_origin, reordered, same_members,
        save_prefs_to, shifted, split_rows,
        split_dock_width, target_line_for_row, use_split, use_two_pane, DiffLineKind, DiffLocation, DiffView,
        Prefs, SplitRow, ToolTab, DIFF_TWO_PANE_MIN, MAX_PANEL_W, MIN_CENTER_W, SPLIT_MIN_W,
    };

    /// Verbatim shape of the noise a new file produces — the case that
    /// prompted this: five metadata lines before any content.
    const NEW_FILE: &str = "\
diff --git a/agent-schema/README.md b/agent-schema/README.md
new file mode 100644
index 0000000..2cf3f44
--- /dev/null
+++ b/agent-schema/README.md
@@ -0,0 +1,67 @@
+# agent-schema — the tool contract
+
";

    #[test]
    fn git_metadata_preamble_is_replaced_by_a_single_file_header() {
        let rows = readable_diff_lines(NEW_FILE);
        assert_eq!(rows[0].kind, DiffLineKind::FileHeader);
        assert_eq!(
            rows[0].text, "agent-schema/README.md",
            "header names the file"
        );

        for noise in [
            "diff --git",
            "new file mode",
            "index 0000000",
            "--- /dev/null",
            "+++ b/",
            "@@",
        ] {
            assert!(
                !rows.iter().any(|r| r.text.contains(noise)),
                "{noise:?} should not be rendered, got {:?}",
                rows.iter().map(|r| &r.text).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn every_content_line_carries_its_real_file_line_number() {
        // This is what replaces the `@@ -0,0 +1,67 @@` header: the coordinates
        // were only ever a way to work out these numbers by hand.
        let rows = readable_diff_lines(NEW_FILE);
        let added: Vec<(Option<u32>, &str)> = rows
            .iter()
            .filter(|r| r.kind == DiffLineKind::Added)
            .map(|r| (r.number, r.text.as_str()))
            .collect();
        assert_eq!(added[0].0, Some(1), "new file starts at line 1");
        assert_eq!(added[0].1, "# agent-schema — the tool contract");
        assert_eq!(added[1].0, Some(2), "and increments");
    }

    #[test]
    fn line_numbers_track_old_and_new_files_independently() {
        // Removed lines number against the OLD file, added and context against
        // the new one — otherwise a deletion shifts every later number.
        let diff = "@@ -10,4 +20,4 @@\n ctx\n-gone\n+added\n ctx2\n";
        let rows = readable_diff_lines(diff);
        let n = |k: DiffLineKind| -> Vec<Option<u32>> {
            rows.iter()
                .filter(|r| r.kind == k)
                .map(|r| r.number)
                .collect()
        };
        assert_eq!(
            n(DiffLineKind::Removed),
            vec![Some(11)],
            "old-file numbering"
        );
        assert_eq!(n(DiffLineKind::Added), vec![Some(21)], "new-file numbering");
        assert_eq!(n(DiffLineKind::Context), vec![Some(20), Some(22)]);
    }

    #[test]
    fn the_hunk_section_heading_survives_but_the_coordinates_do_not() {
        // git puts the enclosing function/heading after the second `@@`. That
        // part is genuinely useful; the numbers are what the gutter now shows.
        let diff = "@@ -1,2 +1,2 @@ fn interesting()\n a\n@@ -50,2 +50,2 @@ fn other()\n b\n";
        let rows = readable_diff_lines(diff);
        let breaks: Vec<&str> = rows
            .iter()
            .filter(|r| r.kind == DiffLineKind::HunkBreak)
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(breaks, vec!["fn interesting()", "fn other()"]);
    }

    #[test]
    fn the_diff_marker_column_is_stripped_from_the_text() {
        // The gutter and the row tint already say what happened; keeping the
        // leading +/- shifts every line against the file's real indentation.
        let diff = "@@ -1,1 +1,1 @@\n-    old_indented()\n+    new_indented()\n";
        let rows = readable_diff_lines(diff);
        let texts: Vec<&str> = rows
            .iter()
            .filter(|r| r.number.is_some())
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(texts, vec!["    old_indented()", "    new_indented()"]);
    }

    #[test]
    fn a_deletion_of_a_dashed_line_is_not_mistaken_for_a_file_marker() {
        // `--- ` is the file-pair marker, but `--foo` is a legitimate removed
        // line. Requiring the space-terminated triple form keeps them apart.
        let diff = "@@ -1,3 +1,1 @@\n---foo\n-  --bar\n+baz\n";
        let removed: Vec<String> = readable_diff_lines(diff)
            .iter()
            .filter(|r| r.kind == DiffLineKind::Removed)
            .map(|r| r.text.clone())
            .collect();
        // The leading `-` marker is stripped; the rest of the line is intact.
        assert_eq!(removed, vec!["--foo", "  --bar"]);
    }

    #[test]
    fn renames_are_stated_in_words_rather_than_dropped() {
        let diff = "diff --git a/old.rs b/new.rs\nsimilarity index 98%\nrename from old.rs\nrename to new.rs\n";
        let rows = readable_diff_lines(diff);
        let text: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(text[0], "new.rs", "header uses the post-rename path");
        assert!(text.contains(&"renamed from old.rs"));
        assert!(!text.iter().any(|l| l.contains("similarity index")));
    }

    #[test]
    fn multiple_files_each_get_a_header() {
        let diff = "diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1 +1 @@\n+x\n\
diff --git a/b.rs b/b.rs\nindex 3..4 100644\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n+y\n";
        let headers: Vec<String> = readable_diff_lines(diff)
            .into_iter()
            .filter(|r| r.kind == DiffLineKind::FileHeader)
            .map(|r| r.text)
            .collect();
        assert_eq!(headers, vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn panel_sizes_are_clamped_so_a_drag_cannot_make_one_unrecoverable() {
        let squashed = Prefs {
            sidebar_width: 0.0,
            diff_width: -50.0,
            ..Prefs::default()
        }
        .sanitized();
        assert!(squashed.sidebar_width >= 180.0);
        assert!(squashed.diff_width >= 220.0);

        let huge = Prefs {
            sidebar_width: 9999.0,
            diff_width: 9999.0,
            ..Prefs::default()
        }
        .sanitized();
        assert!(huge.sidebar_width <= 640.0);
        assert!(huge.diff_width <= MAX_PANEL_W);
    }

    /// Rows for `a\nb\nc` where `b` becomes `B`, plus a trailing insertion.
    fn paired_fixture() -> Vec<super::DiffRow> {
        let diff = "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,4 @@ fn x\n a\n-b\n+B\n c\n+d\n";
        readable_diff_lines(diff)
    }

    #[test]
    fn split_pairs_a_modification_across_the_two_columns() {
        let rows = paired_fixture();
        let split = split_rows(&rows);

        // The file header and hunk rule belong to neither side.
        assert_eq!(split[0], SplitRow::Full(0), "file header spans both columns");
        assert_eq!(split[1], SplitRow::Full(1), "hunk rule spans both columns");

        // Context sits opposite itself.
        let ctx_a = split[2];
        assert_eq!(
            ctx_a,
            SplitRow::Pair { left: Some(2), right: Some(2) },
            "a context line is the same row on both sides"
        );

        // The removal and the addition that follows it become ONE line.
        let modified = split[3];
        let SplitRow::Pair { left, right } = modified else {
            panic!("expected a pair, got {modified:?}");
        };
        let (l, r) = (left.expect("old side"), right.expect("new side"));
        assert_eq!(rows[l].kind, DiffLineKind::Removed);
        assert_eq!(rows[r].kind, DiffLineKind::Added);
        assert_eq!(rows[l].text, "b");
        assert_eq!(rows[r].text, "B");

        // A pure insertion has nothing to sit opposite.
        let inserted = *split.last().unwrap();
        assert!(
            matches!(inserted, SplitRow::Pair { left: None, right: Some(_) }),
            "a pure insertion leaves the old column empty, got {inserted:?}"
        );
    }

    #[test]
    fn an_unbalanced_hunk_pads_the_shorter_side_rather_than_sliding() {
        // Two lines replaced by four: the extra three have no old counterpart.
        let diff = "diff --git a/x.rs b/x.rs\n@@ -1,2 +1,4 @@\n-one\n-two\n+1\n+2\n+3\n+4\n";
        let rows = readable_diff_lines(diff);
        let split = split_rows(&rows);
        let pairs: Vec<SplitRow> = split
            .into_iter()
            .filter(|s| matches!(s, SplitRow::Pair { .. }))
            .collect();

        assert_eq!(pairs.len(), 4, "the longer side sets the line count");
        // Both sides present for the first two, old side empty after that.
        for (i, p) in pairs.iter().enumerate() {
            let SplitRow::Pair { left, right } = *p else { unreachable!() };
            assert!(right.is_some(), "every new line is shown");
            assert_eq!(
                left.is_some(),
                i < 2,
                "line {i}: only the first two have an old counterpart"
            );
        }
    }

    #[test]
    fn every_row_survives_the_split_exactly_once() {
        // The pairing must not drop or duplicate content: a diff you cannot
        // trust to show every line is worse than no split view at all.
        let rows = paired_fixture();
        let mut seen: Vec<usize> = Vec::new();
        for s in split_rows(&rows) {
            match s {
                SplitRow::Full(ix) => seen.push(ix),
                SplitRow::Pair { left, right } => {
                    // Context occupies both cells with ONE row; count it once.
                    match (left, right) {
                        (Some(l), Some(r)) if l == r => seen.push(l),
                        (l, r) => seen.extend(l.into_iter().chain(r)),
                    }
                }
            }
        }
        seen.sort_unstable();
        assert_eq!(
            seen,
            (0..rows.len()).collect::<Vec<_>>(),
            "every parsed row appears exactly once"
        );
    }

    #[test]
    fn a_click_on_a_modified_line_targets_the_new_side() {
        let rows = paired_fixture();
        let split = split_rows(&rows);
        let modified = split[3];
        let SplitRow::Pair { right, .. } = modified else { panic!() };
        assert_eq!(
            modified.anchor(),
            right,
            "comments and edit intent address the file as it stands now"
        );
        // A deletion with no counterpart falls back to the old side.
        let deletion = SplitRow::Pair { left: Some(7), right: None };
        assert_eq!(deletion.anchor(), Some(7));
        assert_eq!(SplitRow::Full(0).anchor(), None, "structure is not clickable");
    }

    #[test]
    fn split_is_a_preference_the_width_can_veto() {
        assert!(use_split(DiffView::Split, SPLIT_MIN_W));
        assert!(use_split(DiffView::Split, 1600.0));
        // Too narrow for two readable columns: draw unified instead.
        assert!(!use_split(DiffView::Split, SPLIT_MIN_W - 1.0));
        // ...but the pref is untouched, so widening restores it.
        assert!(!use_split(DiffView::Unified, 4000.0));
        // A tab is treated as unbounded, like `use_two_pane` does.
        assert!(use_split(DiffView::Split, f32::INFINITY));
    }

    #[test]
    fn the_diff_view_survives_a_save_load_cycle() {
        let dir = std::env::temp_dir().join(format!("harness-prefs-view-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("prefs.json");
        let prefs = Prefs { diff_view: DiffView::Split, ..Prefs::default() };
        save_prefs_to(&path, &prefs);
        assert_eq!(load_prefs(&path).diff_view, DiffView::Split);
    }

    #[test]
    fn a_prefs_file_without_a_diff_view_loads_as_unified() {
        // What every existing install has: no `diff_view` key at all. It must
        // land on the layout it was already showing.
        let dir = std::env::temp_dir().join(format!("harness-prefs-noview-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("prefs.json");
        std::fs::write(
            &path,
            r#"{"dark":true,"sidebar_width":280.0,"diff_width":340.0,"sidebar_open":true,"diff_open":true}"#,
        )
        .unwrap();
        assert_eq!(load_prefs(&path).diff_view, DiffView::Unified);
    }

    #[test]
    fn the_diff_dock_is_capped_by_the_window_not_by_a_constant() {
        // The point of issue #8: on a large display the dock must be
        // draggable far past the old 800 px cap.
        let wide = max_diff_width(3840.0, 280.0, true);
        assert!(wide > 800.0, "a 4K window must allow a dock wider than 800px");
        assert_eq!(wide, 3840.0 - 280.0 - MIN_CENTER_W);

        // ...but never so wide that the center pane disappears.
        assert_eq!(
            max_diff_width(1440.0, 280.0, true),
            1440.0 - 280.0 - MIN_CENTER_W
        );

        // A closed sidebar gives its width back to the dock.
        assert!(max_diff_width(1440.0, 280.0, false) > max_diff_width(1440.0, 280.0, true));

        // A window too small to satisfy everyone still yields a usable range
        // rather than an inverted one.
        assert_eq!(max_diff_width(400.0, 280.0, true), 220.0);
    }

    #[test]
    fn dragging_a_tab_onto_another_puts_it_at_that_position() {
        // Forward: 1 dropped onto 3 lands after the tabs it passed.
        assert_eq!(reordered(&[1, 2, 3, 4], 1, 3), vec![2, 3, 1, 4]);
        // Backward: 4 dropped onto 2 lands where 2 was.
        assert_eq!(reordered(&[1, 2, 3, 4], 4, 2), vec![1, 4, 2, 3]);
    }

    #[test]
    fn dragging_a_project_onto_another_puts_it_at_that_position() {
        let p = |s: &str| std::path::PathBuf::from(s);
        let roots = [p("/a"), p("/b"), p("/c")];
        assert_eq!(reordered(&roots, p("/a"), p("/c")), vec![p("/b"), p("/c"), p("/a")]);
        assert_eq!(reordered(&roots, p("/c"), p("/a")), vec![p("/c"), p("/a"), p("/b")]);
        assert_eq!(reordered(&roots, p("/x"), p("/a")), roots.to_vec());
    }

    #[test]
    fn moving_a_project_up_or_down_swaps_it_with_its_neighbour() {
        assert_eq!(shifted(&[1, 2, 3], &2, true), vec![2, 1, 3]);
        assert_eq!(shifted(&[1, 2, 3], &2, false), vec![1, 3, 2]);
        // Already at the edge, or unknown: unchanged.
        assert_eq!(shifted(&[1, 2, 3], &1, true), vec![1, 2, 3]);
        assert_eq!(shifted(&[1, 2, 3], &3, false), vec![1, 2, 3]);
        assert_eq!(shifted(&[1, 2, 3], &9, true), vec![1, 2, 3]);
    }

    #[test]
    fn a_project_reorder_keeps_projects_the_other_build_added() {
        let p = |s: &str| std::path::PathBuf::from(s);
        // This build loaded a, b, c; the other build has since added x.
        let current = [p("/a"), p("/b"), p("/c")];
        let disk = vec![p("/a"), p("/x"), p("/b"), p("/c")];
        let base = reorder_base(disk, &current);
        assert_eq!(reordered(&base, p("/a"), p("/c")), vec![p("/x"), p("/b"), p("/c"), p("/a")]);
        assert_eq!(shifted(&base, &p("/c"), true), vec![p("/a"), p("/x"), p("/c"), p("/b")]);
        // A missing or unreadable repos.json never empties the list.
        assert_eq!(reorder_base(Vec::new(), &current), current.to_vec());
        assert!(same_members(&[1, 2, 3], &[3, 1, 2]));
        assert!(!same_members(&[1, 2, 3], &[1, 2, 3, 4]));
        assert!(!same_members(&[1, 2], &[1, 3]));
    }

    #[test]
    fn a_task_reorder_keeps_tasks_the_other_build_added() {
        let task = |id: &str| {
            format!(r#"{{"id":"{id}","name":"{id}","slug":"{id}","home_repo":"app","members":[],"created_at":"2026-11-16T00:00:00Z"}}"#)
        };
        let file = |ids: &[&str]| format!(r#"{{"tasks":[{}]}}"#, ids.iter().map(|i| task(i)).collect::<Vec<_>>().join(","));
        let ids = |s: &worktrees::TaskStore| s.tasks().iter().map(|t| t.id.clone()).collect::<Vec<_>>();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tasks.json");

        // This build loaded a, b; the other build has since added x.
        std::fs::write(&path, file(&["a", "b"])).unwrap();
        let current = worktrees::TaskStore::load(&path).unwrap();
        std::fs::write(&path, file(&["a", "b", "x"])).unwrap();
        let moved = move_task_on_disk(&current, "a", "b").unwrap().expect("order changed");
        assert_eq!(ids(&moved), ["b", "a", "x"]);
        assert_eq!(ids(&worktrees::TaskStore::load(&path).unwrap()), ["b", "a", "x"]);

        // Unknown or same ids change nothing on disk.
        assert!(move_task_on_disk(&moved, "a", "a").unwrap().is_none());
        assert!(move_task_on_disk(&moved, "zz", "a").unwrap().is_none());

        // A missing file falls back to this build's list.
        std::fs::remove_file(&path).unwrap();
        let moved = move_task_on_disk(&current, "b", "a").unwrap().expect("order changed");
        assert_eq!(ids(&moved), ["b", "a"]);

        // An unreadable file is reported and left alone.
        std::fs::write(&path, "{not json").unwrap();
        assert!(move_task_on_disk(&current, "b", "a").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{not json");
    }

    #[test]
    fn the_reorder_panel_stays_inside_the_window() {
        let win = (1200.0, 800.0);
        // Room below the click: placed as asked.
        assert_eq!(reorder_panel_origin((100.0, 120.0), win, 3, 420.0), (100.0, 120.0));
        // Near the bottom: lifted so title + 3 rows + Done fit above the edge.
        assert_eq!(reorder_panel_origin((100.0, 760.0), win, 3, 420.0), (100.0, 800.0 - (96.0 + 80.0) - 8.0));
        // A long list counts only its capped height.
        assert_eq!(reorder_panel_origin((100.0, 760.0), win, 50, 420.0), (100.0, 800.0 - (420.0 + 80.0) - 8.0));
        // Off the left or right edge: pulled in by the margin.
        assert_eq!(reorder_panel_origin((-150.0, 120.0), win, 3, 420.0).0, 8.0);
        assert_eq!(reorder_panel_origin((1100.0, 120.0), win, 3, 420.0).0, 1200.0 - 240.0 - 8.0);
        // A window smaller than the panel: top-left margin, never negative.
        assert_eq!(reorder_panel_origin((100.0, 100.0), (200.0, 150.0), 50, 90.0), (8.0, 8.0));
    }

    #[test]
    fn a_tab_dropped_on_itself_or_on_a_stranger_changes_nothing() {
        // Both arise for real: a click gpui reports as a tiny drag, and a drop
        // onto a strip belonging to a different worktree.
        assert_eq!(reordered(&[1, 2, 3], 2, 2), vec![1, 2, 3]);
        assert_eq!(reordered(&[1, 2, 3], 9, 2), vec![1, 2, 3]);
        assert_eq!(reordered(&[1, 2, 3], 2, 9), vec![1, 2, 3]);
        assert_eq!(reordered(&[], 1, 2), Vec::<u64>::new());
    }

    #[test]
    fn the_tab_is_always_two_pane_and_the_dock_only_when_wide_enough() {
        // A tab fills the center, so it always has room; the dock can be dragged
        // down to 220px, where two columns would leave neither readable.
        let u = DiffView::Unified;
        assert!(use_two_pane(DiffLocation::Tab, 0.0, u, 200.0));
        assert!(!use_two_pane(DiffLocation::Dock, 340.0, u, 200.0));
        assert!(!use_two_pane(DiffLocation::Dock, DIFF_TWO_PANE_MIN - 1.0, u, 200.0));
        assert!(use_two_pane(DiffLocation::Dock, DIFF_TWO_PANE_MIN, u, 200.0));
        assert!(use_two_pane(DiffLocation::Dock, 800.0, u, 200.0));
    }

    #[test]
    fn split_gives_up_the_file_list_rather_than_its_own_columns() {
        let s = DiffView::Split;
        // 600 px dock with a 200 px list leaves a 394 px body: too narrow to
        // split, but the dock alone is wide enough — so drop the list.
        assert!(!use_two_pane(DiffLocation::Dock, 600.0, s, 200.0));
        assert!(use_split(s, 600.0), "the accordion body is the whole dock");
        // Wide enough for both: keep the list.
        assert!(use_two_pane(DiffLocation::Dock, 900.0, s, 200.0));
        assert!(use_split(s, 900.0 - 200.0 - 6.0));
        // A tab is never affected.
        assert!(use_two_pane(DiffLocation::Tab, 0.0, s, 200.0));
    }

    #[test]
    fn picking_split_in_a_narrow_dock_widens_it_just_enough() {
        assert_eq!(split_dock_width(380.0, 816.0), Some(SPLIT_MIN_W));
        assert_eq!(split_dock_width(SPLIT_MIN_W, 816.0), None, "already splits");
        assert_eq!(split_dock_width(380.0, 400.0), None, "window too small to help");
    }

    #[test]
    fn an_added_or_context_row_targets_its_own_line() {
        let rows = readable_diff_lines(
            "diff --git a/x.rs b/x.rs\n@@ -1,2 +1,3 @@\n ctx\n+added\n ctx2\n",
        );
        let added = rows.iter().position(|r| r.kind == DiffLineKind::Added).unwrap();
        assert_eq!(target_line_for_row(&rows, added), rows[added].number);
        let ctx = rows.iter().position(|r| r.kind == DiffLineKind::Context).unwrap();
        assert_eq!(target_line_for_row(&rows, ctx), rows[ctx].number);
    }

    #[test]
    fn a_removed_row_targets_the_next_line_that_still_exists() {
        // A removed row carries its OLD file line number, which does not exist
        // in the working file. Jumping to it would land on an unrelated line and
        // look like the feature simply picked the wrong place.
        let rows = readable_diff_lines(
            "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,2 @@\n keep\n-gone\n after\n",
        );
        let removed = rows.iter().position(|r| r.kind == DiffLineKind::Removed).unwrap();
        let after = rows
            .iter()
            .enumerate()
            .find(|(i, r)| *i > removed && r.kind == DiffLineKind::Context)
            .map(|(_, r)| r.number)
            .unwrap();
        assert_eq!(target_line_for_row(&rows, removed), after);
    }

    #[test]
    fn a_trailing_run_of_removed_rows_targets_nothing() {
        // Deleting the tail of a file leaves no following line to jump to.
        // Opening the file with no jump is right; guessing a line is not.
        let rows = readable_diff_lines(
            "diff --git a/x.rs b/x.rs\n@@ -1,3 +1,1 @@\n keep\n-gone\n-also gone\n",
        );
        let removed = rows.iter().position(|r| r.kind == DiffLineKind::Removed).unwrap();
        assert_eq!(target_line_for_row(&rows, removed), None);
    }

    #[test]
    fn a_header_or_hunk_break_targets_nothing_and_an_out_of_range_index_is_safe() {
        let rows = readable_diff_lines("diff --git a/x.rs b/x.rs\n@@ -1,1 +1,1 @@\n keep\n");
        assert_eq!(target_line_for_row(&rows, 0), None, "file header");
        assert_eq!(target_line_for_row(&rows, 9999), None, "out of range");
    }

    #[test]
    fn a_removed_row_does_not_target_a_line_in_the_next_file() {
        // Two files in one diff: a deletion at the end of the first must not
        // reach across the boundary and open the second file's first line.
        let rows = readable_diff_lines(
            "diff --git a/x.rs b/x.rs\n@@ -1,2 +1,1 @@\n keep\n-gone\n\
diff --git a/y.rs b/y.rs\n@@ -1,1 +1,2 @@\n other\n+new\n",
        );
        let removed = rows.iter().position(|r| r.kind == DiffLineKind::Removed).unwrap();
        assert_eq!(target_line_for_row(&rows, removed), None);
    }

    #[test]
    fn a_prefs_file_without_a_diff_location_loads_as_the_dock() {
        // The setting is new; every existing install must keep the dock it has.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(
            &path,
            r#"{"dark":true,"sidebar_width":280.0,"diff_width":340.0,"sidebar_open":true,"diff_open":true}"#,
        )
        .unwrap();
        let prefs = load_prefs(&path);
        assert_eq!(prefs.diff_location, DiffLocation::Dock);
        assert_eq!(prefs.diff_list_width, 280.0);
        assert_eq!(prefs.tool_tab, ToolTab::Review);
    }

    #[test]
    fn the_tool_tab_survives_a_save_load_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Prefs {
            tool_tab: ToolTab::Files,
            ..Prefs::default()
        };
        save_prefs_to(&path, &prefs);
        assert_eq!(load_prefs(&path).tool_tab, ToolTab::Files);
    }

    #[test]
    fn a_prefs_file_without_a_tool_tab_loads_as_review() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        std::fs::write(
            &path,
            r#"{"dark":true,"sidebar_width":280.0,"diff_width":340.0,"sidebar_open":true,"diff_open":true}"#,
        )
        .unwrap();
        assert_eq!(load_prefs(&path).tool_tab, ToolTab::Review);
    }

    #[test]
    fn the_diff_list_width_is_clamped_like_every_other_panel() {
        // A hand-edited or truncated file must not render the list unusable.
        let squashed = Prefs { diff_list_width: -10.0, ..Prefs::default() }.sanitized();
        assert!(squashed.diff_list_width >= 160.0);
        let huge = Prefs { diff_list_width: 9999.0, ..Prefs::default() }.sanitized();
        assert!(huge.diff_list_width <= 560.0);
    }

    #[test]
    fn the_diff_location_survives_a_save_load_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.json");
        let prefs = Prefs { diff_location: DiffLocation::Tab, ..Prefs::default() };
        save_prefs_to(&path, &prefs);
        assert_eq!(load_prefs(&path).diff_location, DiffLocation::Tab);
    }

    #[test]
    fn relocate_root_moves_between_lists_without_duplicating() {
        use super::relocate_root;
        use std::path::PathBuf;
        let mut active = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let mut archived: Vec<PathBuf> = vec![];

        // Archive /a: leaves active, lands in archived.
        assert!(relocate_root(
            &mut active,
            &mut archived,
            std::path::Path::new("/a")
        ));
        assert_eq!(active, vec![PathBuf::from("/b")]);
        assert_eq!(archived, vec![PathBuf::from("/a")]);

        // Absent root: no-op, reports false.
        assert!(!relocate_root(
            &mut active,
            &mut archived,
            std::path::Path::new("/zzz")
        ));

        // Restore /a back, then a redundant restore can't duplicate it.
        assert!(relocate_root(
            &mut archived,
            &mut active,
            std::path::Path::new("/a")
        ));
        assert!(!relocate_root(
            &mut archived,
            &mut active,
            std::path::Path::new("/a")
        ));
        assert_eq!(
            active
                .iter()
                .filter(|p| p.as_path() == std::path::Path::new("/a"))
                .count(),
            1
        );
    }
}
