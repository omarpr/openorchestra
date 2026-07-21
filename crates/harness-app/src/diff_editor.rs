//! The diff body, rendered in a **read-only zed `Editor`**.
//!
//! The hand-rolled `div`-per-row version could not do what a diff viewer most
//! needs: select an arbitrary piece of text and copy it. gpui has no
//! selectable-text primitive — its own markdown crate hand-rolls one over a
//! custom element — so the honest way to get selection, ⌘C, ⌘F, and real
//! two-axis scrolling is to use the thing that already implements all of them.
//! Zed renders its own diffs in editors for the same reason.
//!
//! ## What this trades away, and what replaces it
//!
//! An editor's gutter numbers the *buffer*, so it would count the diff's own
//! lines rather than the file's — actively misleading. The gutter is therefore
//! **off**, and the per-line numbers move into the hunk headers, which now read
//! `── lines 112-119 · fn interesting() ──`. Positional information survives at
//! the granularity you actually navigate by, and no line-number text pollutes a
//! copied selection.
//!
//! Getting real per-line numbers back means a `MultiBuffer` excerpting the
//! actual files with `BufferDiff` hunks — how zed's own project diff works, and
//! a much larger piece of work.

use gpui::{App, AppContext as _, Entity, Hsla, Window};
use language::{Buffer, LanguageRegistry};
use std::sync::Arc;

use crate::workspace::{readable_diff_lines, DiffLineKind, DiffRow};

/// Marker types for the two row-highlight layers. `highlight_rows` keys its
/// storage by `TypeId`, so added and removed lines need distinct types or the
/// second call replaces the first.
enum AddedRows {}
enum RemovedRows {}

fn added_bg(cx: &App) -> Hsla {
    crate::theme::tint(crate::theme::theme(cx).green, 0.13).into()
}

fn removed_bg(cx: &App) -> Hsla {
    crate::theme::tint(crate::theme::theme(cx).red, 0.13).into()
}

/// Render diff rows to the text the editor will hold, and report which display
/// rows are additions and which are removals.
///
/// Returns `(text, added_rows, removed_rows)` with row indices 0-based, which
/// is what `highlight_rows` anchors against.
pub fn render_text(rows: &[DiffRow]) -> (String, Vec<u32>, Vec<u32>) {
    let mut lines: Vec<String> = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    // Buffer the pending hunk range so a header can state where it starts.
    for row in rows {
        let ix = lines.len() as u32;
        match row.kind {
            DiffLineKind::FileHeader => lines.push(format!("── {} ──", row.text)),
            DiffLineKind::HunkBreak => {
                // The header carries the position now that lines do not.
                let label = if row.text.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", row.text)
                };
                lines.push(format!(
                    "── line {}{label} ──",
                    row.number.map(|n| n.to_string()).unwrap_or_default()
                ));
            }
            DiffLineKind::Added => {
                added.push(ix);
                lines.push(format!("+ {}", row.text));
            }
            DiffLineKind::Removed => {
                removed.push(ix);
                lines.push(format!("- {}", row.text));
            }
            DiffLineKind::Context => lines.push(format!("  {}", row.text)),
        }
    }
    (lines.join("\n"), added, removed)
}

/// Build a read-only editor showing `unified`.
pub fn build(
    unified: &str,
    languages: Option<Arc<LanguageRegistry>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<editor::Editor> {
    let rows = readable_diff_lines(unified);
    let (text, added, removed) = render_text(&rows);

    // No language is set: a unified diff is not any one file's syntax, and
    // highlighting it as Rust would colour `-` removals as operators.
    let buffer = cx.new(|cx| Buffer::local(text, cx));
    let _ = languages;

    cx.new(|cx| {
        let mut editor = editor::Editor::for_buffer(buffer.clone(), None, window, cx);
        // Read-only, but still selectable and copyable — which is the point.
        editor.set_read_only(true);
        // Buffer-relative numbers would count diff lines, not file lines.
        editor.set_show_line_numbers(false, cx);
        editor.set_show_gutter(false, cx);
        // No soft wrap: long lines scroll horizontally instead of reflowing,
        // which keeps code aligned and is why the old view wanted both axes.
        editor.set_soft_wrap_mode(
            language::language_settings::SoftWrap::None,
            cx,
        );

        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let mut highlight = |rows: &[u32], color: fn(&App) -> Hsla, added: bool| {
            for &row in rows {
                let start = snapshot.anchor_before(language::Point::new(row, 0));
                let end = snapshot.anchor_after(language::Point::new(row, 0));
                if added {
                    editor.highlight_rows::<AddedRows>(
                        start..end,
                        color,
                        editor::RowHighlightOptions::default(),
                        cx,
                    );
                } else {
                    editor.highlight_rows::<RemovedRows>(
                        start..end,
                        color,
                        editor::RowHighlightOptions::default(),
                        cx,
                    );
                }
            }
        };
        highlight(&added, added_bg, true);
        highlight(&removed, removed_bg, false);
        editor
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::readable_diff_lines;

    const DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -10,3 +20,3 @@ fn interesting()
 kept
-gone
+added
";

    #[test]
    fn rows_render_with_markers_and_no_line_numbers() {
        // Line numbers must NOT appear in the text: they would be copied along
        // with any full-line selection, which is what this rewrite exists to
        // make clean.
        let (text, _, _) = render_text(&readable_diff_lines(DIFF));
        assert!(text.contains("  kept"), "{text}");
        assert!(text.contains("- gone"), "{text}");
        assert!(text.contains("+ added"), "{text}");
        for line in text.lines().filter(|l| !l.starts_with("──")) {
            assert!(
                !line.trim_start().starts_with(char::is_numeric),
                "line numbers leaked into copyable text: {line:?}"
            );
        }
    }

    #[test]
    fn the_hunk_header_carries_the_position_the_gutter_no_longer_shows() {
        let (text, _, _) = render_text(&readable_diff_lines(DIFF));
        let header = text
            .lines()
            .find(|l| l.contains("line 20"))
            .expect("hunk header names its starting line");
        assert!(header.contains("fn interesting()"), "{header}");
    }

    #[test]
    fn added_and_removed_rows_are_reported_for_highlighting() {
        let (text, added, removed) = render_text(&readable_diff_lines(DIFF));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(added.len(), 1);
        assert_eq!(removed.len(), 1);
        assert!(lines[added[0] as usize].starts_with("+ "));
        assert!(lines[removed[0] as usize].starts_with("- "));
    }

    #[test]
    fn the_file_header_survives_as_a_readable_rule() {
        let (text, _, _) = render_text(&readable_diff_lines(DIFF));
        assert!(text.starts_with("── src/a.rs ──"), "{text}");
    }
}
