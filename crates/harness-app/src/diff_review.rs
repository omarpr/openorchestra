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

use std::collections::HashMap;
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

/// All review notes, keyed by worktree id.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReviewNotes {
    #[serde(default)]
    by_worktree: HashMap<String, Vec<Comment>>,
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
    pub fn clear(&mut self, worktree: &str) {
        self.by_worktree.remove(worktree);
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
