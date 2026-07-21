//! Inline review comments on the diff (§3.4).
//!
//! The point of leaving a note on a diff line, in a harness whose whole premise
//! is an agent doing the work, is to hand it back to the agent. So comments are
//! not just annotations: [`ReviewNotes::as_review`] renders them as a single
//! block of feedback that can be pasted straight into the agent's terminal or
//! chat, addressed by file and line.
//!
//! Persisted per worktree in the harness state dir — *not* in the repo, for the
//! same reason the adapters' files are excluded from git: review-in-progress is
//! the user's working state, not a change to their project.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One note attached to a line of one file's diff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    /// Repo-relative path, matching the diff's file list.
    pub file: String,
    /// The file line the note is against, as shown in the diff gutter.
    pub line: u32,
    pub body: String,
}

/// All review state, keyed by worktree id: the comments, and which files have
/// been signed off.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewNotes {
    #[serde(default)]
    by_worktree: HashMap<String, Vec<Comment>>,
    /// Files marked reviewed. A `BTreeSet` so the persisted file has stable
    /// ordering and diffs of it stay readable.
    #[serde(default)]
    reviewed: HashMap<String, BTreeSet<String>>,
}

impl ReviewNotes {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Atomic write, like every other persisted file here: a half-written
    /// notes file would lose a review rather than fail loudly.
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, path);
        }
    }

    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join("review-notes.json")
    }

    /// Where a worktree's exported markdown review is written.
    ///
    /// Under the harness state dir, never the repo: an exported review is the
    /// user's own notes, and dropping an untracked file into their project is
    /// what the git-exclude work exists to avoid. The worktree id contains a
    /// `/`, so it is flattened rather than creating a stray directory.
    pub fn markdown_path_in(state_dir: &Path, worktree: &str) -> PathBuf {
        let stem: String = worktree
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
            .collect();
        state_dir.join("reviews").join(format!("{stem}.md"))
    }

    /// Add or replace the note on `(file, line)`.
    ///
    /// One note per line, replaced rather than appended: a second thought about
    /// a line is a correction, and stacking them would make the review read as
    /// a conversation with yourself.
    pub fn set(&mut self, worktree: &str, file: &str, line: u32, body: &str) {
        let notes = self.by_worktree.entry(worktree.to_string()).or_default();
        notes.retain(|c| !(c.file == file && c.line == line));
        if !body.trim().is_empty() {
            notes.push(Comment {
                file: file.to_string(),
                line,
                body: body.trim().to_string(),
            });
        }
        // Keep file order stable and lines ascending, so the rendered review
        // reads top-to-bottom like the diff it came from.
        notes.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    }

    pub fn get(&self, worktree: &str, file: &str, line: u32) -> Option<&Comment> {
        self.by_worktree
            .get(worktree)?
            .iter()
            .find(|c| c.file == file && c.line == line)
    }

    pub fn count(&self, worktree: &str) -> usize {
        self.by_worktree.get(worktree).map_or(0, |n| n.len())
    }

    /// Drop every note for a worktree — after the review has been handed off.
    ///
    /// Also clears the reviewed marks: they describe a pass over a particular
    /// set of changes, and keeping them after discarding the comments would
    /// claim a review that no longer exists.
    pub fn clear(&mut self, worktree: &str) {
        self.by_worktree.remove(worktree);
        self.reviewed.remove(worktree);
    }

    /// Whether `file` has been marked reviewed in this worktree.
    pub fn is_reviewed(&self, worktree: &str, file: &str) -> bool {
        self.reviewed
            .get(worktree)
            .is_some_and(|files| files.contains(file))
    }

    pub fn set_reviewed(&mut self, worktree: &str, file: &str, reviewed: bool) {
        let files = self.reviewed.entry(worktree.to_string()).or_default();
        if reviewed {
            files.insert(file.to_string());
        } else {
            files.remove(file);
        }
    }

    /// How many of `files` are marked reviewed — for the "3 of 8" counter.
    pub fn reviewed_count<'a>(
        &self,
        worktree: &str,
        files: impl IntoIterator<Item = &'a str>,
    ) -> usize {
        let Some(marked) = self.reviewed.get(worktree) else {
            return 0;
        };
        // Counted against the files actually in the diff, so a mark left over
        // from a file that has since stopped changing does not inflate it.
        files.into_iter().filter(|f| marked.contains(*f)).count()
    }

    /// Render the notes as a markdown document, for saving alongside a PR or
    /// pasting somewhere that renders it.
    pub fn as_markdown(&self, worktree: &str, branch: &str) -> String {
        let Some(notes) = self.by_worktree.get(worktree).filter(|n| !n.is_empty()) else {
            return String::new();
        };
        let mut out = format!("# Review — {branch}\n");
        let mut current_file: Option<&str> = None;
        for note in notes {
            if current_file != Some(note.file.as_str()) {
                out.push_str(&format!("\n## `{}`\n\n", note.file));
                current_file = Some(&note.file);
            }
            // A list item per note, line number bolded so it survives being
            // skimmed in a rendered view.
            out.push_str(&format!("- **line {}** — {}\n", note.line, note.body));
        }
        out
    }

    /// Render the notes as feedback to hand to the agent.
    ///
    /// Grouped by file with explicit line numbers, because that is what an
    /// agent needs to act on a comment without guessing where it points.
    pub fn as_review(&self, worktree: &str) -> String {
        let Some(notes) = self.by_worktree.get(worktree).filter(|n| !n.is_empty()) else {
            return String::new();
        };
        let mut out = String::from("Review comments:\n");
        let mut current_file: Option<&str> = None;
        for note in notes {
            if current_file != Some(note.file.as_str()) {
                out.push_str(&format!("\n{}\n", note.file));
                current_file = Some(&note.file);
            }
            out.push_str(&format!("  line {}: {}\n", note.line, note.body));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes() -> ReviewNotes {
        let mut n = ReviewNotes::default();
        n.set("app/nav", "src/b.rs", 20, "second file");
        n.set("app/nav", "src/a.rs", 9, "later line");
        n.set("app/nav", "src/a.rs", 4, "earlier line");
        n
    }

    #[test]
    fn notes_are_ordered_by_file_then_line() {
        // The review should read top-to-bottom like the diff it came from.
        let review = notes().as_review("app/nav");
        let a4 = review.find("line 4").unwrap();
        let a9 = review.find("line 9").unwrap();
        let b20 = review.find("line 20").unwrap();
        assert!(a4 < a9 && a9 < b20, "out of order:\n{review}");
    }

    #[test]
    fn a_second_note_on_a_line_replaces_the_first() {
        // A correction, not a conversation with yourself.
        let mut n = notes();
        n.set("app/nav", "src/a.rs", 4, "rewritten");
        assert_eq!(n.get("app/nav", "src/a.rs", 4).unwrap().body, "rewritten");
        assert_eq!(n.count("app/nav"), 3, "replaced, not appended");
    }

    #[test]
    fn an_empty_body_deletes_the_note() {
        // Clearing the input is the delete affordance — there is deliberately
        // no separate remove(), since a second path would be a second way for
        // the two to disagree. It must also not persist an empty note that
        // renders as a blank box forever.
        let mut n = notes();
        n.set("app/nav", "src/a.rs", 4, "   ");
        assert!(n.get("app/nav", "src/a.rs", 4).is_none());
        assert_eq!(n.count("app/nav"), 2);
    }

    #[test]
    fn notes_are_scoped_to_their_worktree() {
        let n = notes();
        assert_eq!(n.count("app/nav"), 3);
        assert_eq!(n.count("app/other"), 0);
        assert!(n.as_review("app/other").is_empty());
    }

    #[test]
    fn the_review_names_each_file_once_with_its_lines_under_it() {
        let review = notes().as_review("app/nav");
        assert_eq!(review.matches("src/a.rs").count(), 1, "{review}");
        assert!(review.contains("  line 4: earlier line"));
        assert!(review.contains("Review comments:"));
    }

    #[test]
    fn an_empty_review_is_empty_not_a_header_with_nothing_under_it() {
        assert!(ReviewNotes::default().as_review("app/nav").is_empty());
        assert!(ReviewNotes::default().as_markdown("app/nav", "feat/x").is_empty());
    }

    #[test]
    fn markdown_groups_by_file_under_headings() {
        let md = notes().as_markdown("app/nav", "feat/nav");
        assert!(md.starts_with("# Review — feat/nav"), "{md}");
        assert_eq!(md.matches("## `src/a.rs`").count(), 1, "{md}");
        assert!(md.contains("- **line 4** — earlier line"));
        // Ordering matches the plain-text review and therefore the diff.
        assert!(md.find("src/a.rs").unwrap() < md.find("src/b.rs").unwrap());
    }

    #[test]
    fn notes_survive_a_save_load_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = ReviewNotes::path_in(dir.path());
        notes().save(&path);

        let loaded = ReviewNotes::load(&path);
        assert_eq!(loaded.count("app/nav"), 3);
        assert_eq!(
            loaded.get("app/nav", "src/a.rs", 9).unwrap().body,
            "later line"
        );
        // Atomic write leaves no residue.
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "review-notes.json")
            .collect();
        assert!(strays.is_empty(), "left {strays:?} behind");
    }

    #[test]
    fn the_markdown_export_lands_outside_the_repo_with_a_flat_filename() {
        // A worktree id is "repo/slug"; left alone that `/` would create a
        // directory, and worse, a path component the user did not ask for.
        let path = ReviewNotes::markdown_path_in(Path::new("/state"), "app/nav");
        assert_eq!(path, Path::new("/state/reviews/app-nav.md"));
        assert!(!path.to_string_lossy().contains("app/nav"));
    }

    #[test]
    fn the_full_review_cycle_survives_add_export_and_clear() {
        // Pins the three affordances the toolbar exposes, so a future refactor
        // that drops one fails here rather than in a screenshot.
        let dir = tempfile::tempdir().unwrap();
        let notes_path = ReviewNotes::path_in(dir.path());
        let mut n = ReviewNotes::default();

        n.set("app/nav", "src/a.rs", 12, "this leaks");
        n.save(&notes_path);
        assert_eq!(ReviewNotes::load(&notes_path).count("app/nav"), 1);

        // copy — plain feedback for the agent
        assert!(n.as_review("app/nav").contains("line 12: this leaks"));
        // save .md — a document for a human
        assert!(n.as_markdown("app/nav", "feat/nav").contains("- **line 12** — this leaks"));
        // clear
        n.clear("app/nav");
        n.save(&notes_path);
        assert_eq!(ReviewNotes::load(&notes_path).count("app/nav"), 0);
    }

    #[test]
    fn files_can_be_marked_reviewed_and_unmarked() {
        let mut n = notes();
        assert!(!n.is_reviewed("app/nav", "src/a.rs"));
        n.set_reviewed("app/nav", "src/a.rs", true);
        assert!(n.is_reviewed("app/nav", "src/a.rs"));
        assert!(!n.is_reviewed("app/other", "src/a.rs"), "scoped per worktree");
        n.set_reviewed("app/nav", "src/a.rs", false);
        assert!(!n.is_reviewed("app/nav", "src/a.rs"));
    }

    #[test]
    fn reviewed_count_only_counts_files_still_in_the_diff() {
        // A file marked reviewed and then reverted should not keep inflating
        // "N of M reviewed" for changes that no longer exist.
        let mut n = notes();
        n.set_reviewed("app/nav", "src/a.rs", true);
        n.set_reviewed("app/nav", "gone.rs", true);
        assert_eq!(n.reviewed_count("app/nav", ["src/a.rs", "src/b.rs"]), 1);
    }

    #[test]
    fn reviewed_marks_survive_a_save_load_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = ReviewNotes::path_in(dir.path());
        let mut n = notes();
        n.set_reviewed("app/nav", "src/a.rs", true);
        n.save(&path);
        assert!(ReviewNotes::load(&path).is_reviewed("app/nav", "src/a.rs"));
    }

    #[test]
    fn clearing_a_review_also_clears_its_reviewed_marks() {
        // The marks describe a pass over a particular set of changes; keeping
        // them after discarding the comments claims a review that is gone.
        let mut n = notes();
        n.set_reviewed("app/nav", "src/a.rs", true);
        n.clear("app/nav");
        assert!(!n.is_reviewed("app/nav", "src/a.rs"));
    }

    #[test]
    fn a_missing_or_corrupt_file_loads_as_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(ReviewNotes::load(&dir.path().join("nope.json")).count("x"), 0);
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, "{not json").unwrap();
        assert_eq!(ReviewNotes::load(&bad).count("x"), 0);
    }

    #[test]
    fn clearing_a_worktree_drops_only_its_own_notes() {
        let mut n = notes();
        n.set("app/other", "src/z.rs", 1, "keep me");
        n.clear("app/nav");
        assert_eq!(n.count("app/nav"), 0);
        assert_eq!(n.count("app/other"), 1);
    }
}
