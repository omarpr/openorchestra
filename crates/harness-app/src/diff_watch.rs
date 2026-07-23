//! Per-selection filesystem watch that drives live diff refresh (Deliverable A).
//!
//! The diff panel was event-driven only — it recomputed on selection and on
//! structural mutations, so while an agent edited files it stayed frozen until
//! you re-clicked the worktree. This module watches the *currently-selected*
//! worktree/main root and signals the workspace to recompute, so the diff keeps
//! up with the agent in near real time.
//!
//! Cost is bounded to the one diff on screen: only the selected root is watched,
//! and the watch is retargeted when the selection changes. Events are filtered
//! here ([`is_relevant_change`]) and coalesced by the workspace's drain cadence —
//! that cadence *is* the debounce, so a burst of writes in one agent turn
//! collapses to a single recompute, which the existing `diff_gen` counter then
//! keeps from piling up.

use std::path::Path;

/// Whether a changed path should trigger a diff recompute.
///
/// Ignores VCS metadata and build output. A `cargo build` churns `target/`
/// continuously and would otherwise storm the diff. Git already excludes ignored
/// files from its diff output, so a miss here is only a wasted (deduped,
/// debounced) recompute, never a wrong diff — the filter is about cost, not
/// correctness.
pub fn is_relevant_change(path: &Path) -> bool {
    const IGNORED: [&str; 3] = [".git", "target", "node_modules"];
    !path
        .components()
        .any(|c| IGNORED.contains(&c.as_os_str().to_string_lossy().as_ref()))
}

#[cfg(test)]
mod tests {
    use super::is_relevant_change;
    use std::path::Path;

    #[test]
    fn build_and_vcs_dirs_are_ignored() {
        assert!(!is_relevant_change(Path::new("/repo/.git/index")));
        assert!(!is_relevant_change(Path::new("/repo/target/debug/foo")));
        assert!(!is_relevant_change(Path::new("/repo/node_modules/x/y.js")));
        assert!(!is_relevant_change(Path::new("/repo/crates/target/x"))); // nested target
    }

    #[test]
    fn source_files_are_relevant() {
        assert!(is_relevant_change(Path::new("/repo/src/main.rs")));
        assert!(is_relevant_change(Path::new("/repo/Cargo.toml")));
        // A file literally named `target` (not a dir component) is still relevant.
        assert!(is_relevant_change(Path::new("/repo/src/target.rs")));
    }
}
