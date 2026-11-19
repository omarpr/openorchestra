//! Persisted record of which tabs were open per worktree, so terminal/agent
//! tabs can be reattached to their tmux sessions on reopen. Same load/save
//! shape as prefs/repos (a JSON file under the app state dir).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use agent_runtime::AgentKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct PanesState {
    /// Monotonic, never-reused tab-id source. Stable ids let a tab keep the
    /// same tmux session name across restarts.
    #[serde(default)]
    pub next_tab_id: u64,
    #[serde(default)]
    pub worktrees: BTreeMap<String, WorktreePanes>,
    /// Keys this process deleted, so a merge-on-save does not resurrect them
    /// from the copy on disk. In-memory only.
    #[serde(skip)]
    pub removed: BTreeSet<String>,
}

impl PanesState {
    /// Drop a worktree's records and remember the deletion for [`save`].
    pub fn remove(&mut self, key: &str) -> Option<WorktreePanes> {
        self.removed.insert(key.to_string());
        self.worktrees.remove(key)
    }

    /// Fold another process's view (the file on disk) into ours. Two app
    /// instances (an installed build and a dev build) share this file; a plain
    /// overwrite let whichever saved last erase the other's tabs and roll back
    /// the id counter, so the next id handed out could reuse a live session's
    /// name. Ours wins per key; keys only on disk are kept unless we deleted
    /// them; the id counter never goes backwards.
    pub fn merge_from(&mut self, disk: PanesState) {
        self.next_tab_id = self.next_tab_id.max(disk.next_tab_id);
        for (key, wp) in disk.worktrees {
            if !self.removed.contains(&key) {
                self.worktrees.entry(key).or_insert(wp);
            }
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorktreePanes {
    #[serde(default)]
    pub active: Option<u64>,
    #[serde(default)]
    pub tabs: Vec<TabRecord>,
    /// The agent this pane is meant to be running, kept even when its tab
    /// came back as a plain shell (the agent died with tmux on a computer
    /// restart, or its tab was closed). A task pane with this set and no live
    /// agent shows STOPPED with a Restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TabRecord {
    pub id: u64,
    pub kind: TabRecordKind,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TabRecordKind {
    Shell,
    Agent(AgentKind),
    Editor,
    Chat,
    Diff,
    ReviewOverview(Option<Arc<crate::review_overview::StoredReviewOverview>>),
}

/// Rewrite legacy `task:{id}` pane keys to `task:{id}:member:{home_worktree_id}`.
///
/// Returns the number of keys migrated. Idempotent.
pub fn migrate_task_pane_keys(state: &mut PanesState, home_by_task: &[(String, String)]) -> usize {
    let mut moved = 0;
    for (task_id, home_wt) in home_by_task {
        let legacy = format!("task:{task_id}");
        if let Some(wp) = state.remove(&legacy) {
            let neu = a2a_broker::task_member_pane_key(task_id, home_wt);
            // Prefer existing new-key data if both somehow exist.
            state.worktrees.entry(neu).or_insert(wp);
            moved += 1;
        }
    }
    moved
}

/// Review overview tabs live beside `panes.json`. An installed build and a dev
/// build share `panes.json`; a tab kind the older build cannot deserialize
/// makes `load` return an empty state, and that build's next save then drops
/// the other instance's tabs and the tab-id counter.
const REVIEW_TABS_FILE: &str = "review-tabs.json";

fn review_tabs_path(panes_path: &Path) -> std::path::PathBuf {
    panes_path.with_file_name(REVIEW_TABS_FILE)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReviewTabsFile {
    #[serde(default)]
    worktrees: BTreeMap<String, ReviewWorktreeTabs>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ReviewWorktreeTabs {
    /// Set when the active tab is a review overview. Absent when a legacy tab
    /// is active, so a reload does not override that choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<u64>,
    tabs: Vec<PlacedReviewTab>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PlacedReviewTab {
    index: usize,
    record: TabRecord,
}

fn legacy_and_reviews(state: &PanesState) -> (BTreeMap<String, WorktreePanes>, ReviewTabsFile) {
    let mut legacy = BTreeMap::new();
    let mut reviews = ReviewTabsFile::default();
    for (key, wp) in &state.worktrees {
        let mut tabs = Vec::new();
        let mut placed = Vec::new();
        for (index, tab) in wp.tabs.iter().enumerate() {
            if matches!(tab.kind, TabRecordKind::ReviewOverview(_)) {
                placed.push(PlacedReviewTab {
                    index,
                    record: tab.clone(),
                });
            } else {
                tabs.push(tab.clone());
            }
        }
        let review_active = wp
            .active
            .filter(|id| placed.iter().any(|tab| tab.record.id == *id));
        // An active id the older build's tab list does not contain is how a
        // later save of that build loses track of which tab was open.
        let active = if review_active.is_some() {
            tabs.first().map(|tab| tab.id)
        } else {
            wp.active.filter(|id| tabs.iter().any(|tab| tab.id == *id))
        };
        legacy.insert(
            key.clone(),
            WorktreePanes {
                active,
                tabs,
                agent: wp.agent.clone(),
            },
        );
        if !placed.is_empty() {
            reviews.worktrees.insert(
                key.clone(),
                ReviewWorktreeTabs {
                    active: review_active,
                    tabs: placed,
                },
            );
        }
    }
    (legacy, reviews)
}

fn splice_review_tabs(state: &mut PanesState, path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(file) = serde_json::from_str::<ReviewTabsFile>(&text) else {
        return;
    };
    for (key, review) in file.worktrees {
        let wp = state.worktrees.entry(key).or_default();
        let mut placed = review.tabs;
        placed.sort_by_key(|tab| tab.index);
        for tab in placed {
            if wp.tabs.iter().any(|existing| existing.id == tab.record.id) {
                continue;
            }
            let at = tab.index.min(wp.tabs.len());
            wp.tabs.insert(at, tab.record);
        }
        if let Some(active) = review.active
            && wp.tabs.iter().any(|tab| tab.id == active)
        {
            wp.active = Some(active);
        }
    }
}

pub fn load(path: &Path) -> PanesState {
    let mut state = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PanesState::default(),
    };
    splice_review_tabs(&mut state, &review_tabs_path(path));
    state
}

/// Merge with the file on disk (see [`PanesState::merge_from`]), then write.
///
/// Review overview tabs are not part of the shared file. See [`REVIEW_TABS_FILE`].
pub fn save(path: &Path, state: &mut PanesState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    state.merge_from(load(path));
    let (worktrees, reviews) = legacy_and_reviews(state);
    let legacy = PanesState {
        next_tab_id: state.next_tab_id,
        worktrees,
        removed: BTreeSet::new(),
    };
    match serde_json::to_string_pretty(&legacy) {
        Ok(json) => {
            if let Err(e) = crate::state_files::write_private(path, json.as_bytes()) {
                log::warn!("could not write panes state to {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("could not serialize panes state: {e}"),
    }
    let review_path = review_tabs_path(path);
    if reviews.worktrees.is_empty() {
        let _ = std::fs::remove_file(&review_path);
        return;
    }
    match serde_json::to_string_pretty(&reviews) {
        Ok(json) => {
            if let Err(e) = crate::state_files::write_private(&review_path, json.as_bytes()) {
                log::warn!(
                    "could not write review tabs to {}: {e}",
                    review_path.display()
                );
            }
        }
        Err(e) => log::warn!("could not serialize review tabs: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");

        let mut state = PanesState {
            next_tab_id: 5,
            ..Default::default()
        };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(3),
                tabs: vec![
                    TabRecord {
                        id: 3,
                        kind: TabRecordKind::Agent(AgentKind::Pi),
                        title: "pi".into(),
                    },
                    TabRecord {
                        id: 4,
                        kind: TabRecordKind::Shell,
                        title: "shell".into(),
                    },
                ],
                agent: None,
            },
        );

        save(&path, &mut state);
        let back = load(&path);
        assert_eq!(back, state);
    }

    #[test]
    fn a_diff_tab_round_trips_through_disk() {
        // The diff tab is a view over state the app already holds, but WHICH
        // tabs are open is persisted like any other, so reopening restores the
        // layout the user left.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = PanesState {
            next_tab_id: 2,
            ..Default::default()
        };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(1),
                tabs: vec![TabRecord {
                    id: 1,
                    kind: TabRecordKind::Diff,
                    title: "⑂ diff".into(),
                }],
                agent: None,
            },
        );
        save(&path, &mut state);
        assert_eq!(load(&path), state);
    }

    #[test]
    fn review_overview_restores_its_active_tab_and_last_completed_report() {
        use crate::review_overview::{
            ReviewCli, ReviewPreferences, ReviewReport, StoredReviewOverview,
        };

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = PanesState {
            next_tab_id: 3,
            ..Default::default()
        };
        let saved = StoredReviewOverview {
            preferences: ReviewPreferences {
                cli: ReviewCli::Codex,
                models: BTreeMap::from([(ReviewCli::Codex, "test-model".to_string())]),
            },
            report: ReviewReport {
                markdown: "## Riskiest change\n\nA test report.".into(),
                scope: vec!["project · branch · vs main".into()],
                limitations: Vec::new(),
                files: Vec::new(),
                generated: true,
            },
        };
        state.worktrees.insert(
            "main:project".into(),
            WorktreePanes {
                active: Some(2),
                tabs: vec![
                    TabRecord {
                        id: 1,
                        kind: TabRecordKind::Shell,
                        title: "shell".into(),
                    },
                    TabRecord {
                        id: 2,
                        kind: TabRecordKind::ReviewOverview(Some(Arc::new(saved))),
                        title: "Review Overview".into(),
                    },
                ],
                agent: None,
            },
        );
        save(&path, &mut state);
        assert_eq!(load(&path), state);
    }

    #[test]
    fn review_overview_stays_out_of_the_file_an_older_build_reads() {
        use crate::review_overview::{ReviewPreferences, ReviewReport, StoredReviewOverview};

        #[derive(Deserialize)]
        enum OldKind {
            Shell,
            Agent(AgentKind),
            Editor,
            Chat,
            Diff,
        }
        #[derive(Deserialize)]
        struct OldTab {
            id: u64,
            kind: OldKind,
            title: String,
        }
        #[derive(Deserialize)]
        struct OldWp {
            #[serde(default)]
            active: Option<u64>,
            #[serde(default)]
            tabs: Vec<OldTab>,
        }
        #[derive(Deserialize)]
        struct OldPanes {
            #[serde(default)]
            next_tab_id: u64,
            #[serde(default)]
            worktrees: BTreeMap<String, OldWp>,
        }

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = PanesState {
            next_tab_id: 9,
            ..Default::default()
        };
        let saved = StoredReviewOverview {
            preferences: ReviewPreferences::default(),
            report: ReviewReport {
                markdown: "kept".into(),
                scope: Vec::new(),
                limitations: Vec::new(),
                files: Vec::new(),
                generated: true,
            },
        };
        state.worktrees.insert(
            "main:project".into(),
            WorktreePanes {
                active: Some(2),
                tabs: vec![
                    TabRecord {
                        id: 1,
                        kind: TabRecordKind::Shell,
                        title: "shell".into(),
                    },
                    TabRecord {
                        id: 2,
                        kind: TabRecordKind::ReviewOverview(Some(Arc::new(saved))),
                        title: "Review Overview".into(),
                    },
                ],
                agent: Some(AgentKind::Codex),
            },
        );
        save(&path, &mut state);

        let shared = std::fs::read_to_string(&path).unwrap();
        assert!(!shared.contains("ReviewOverview"), "{shared}");
        let old: OldPanes = serde_json::from_str(&shared).expect("older build can read panes.json");
        assert_eq!(old.next_tab_id, 9);
        let wp = old.worktrees.get("main:project").unwrap();
        assert_eq!(wp.tabs.len(), 1);
        assert_eq!(wp.tabs[0].id, 1);
        assert_eq!(wp.tabs[0].title, "shell");
        match &wp.tabs[0].kind {
            OldKind::Shell => {}
            OldKind::Agent(kind) => panic!("a shell tab was read as {kind}"),
            OldKind::Editor | OldKind::Chat | OldKind::Diff => panic!("a shell tab changed kind"),
        }
        // The active id in the shared file names a tab that file actually has.
        assert_eq!(wp.active, Some(1));

        let back = load(&path);
        assert_eq!(back.next_tab_id, 9);
        let restored = back.worktrees.get("main:project").unwrap();
        assert_eq!(restored.active, Some(2));
        assert_eq!(restored.tabs.len(), 2);
        assert!(matches!(
            restored.tabs[1].kind,
            TabRecordKind::ReviewOverview(_)
        ));
    }

    #[test]
    fn migrate_task_pane_keys_rewrites_legacy() {
        let mut state = PanesState {
            next_tab_id: 2,
            ..Default::default()
        };
        state.worktrees.insert(
            "task:auth".into(),
            WorktreePanes {
                active: Some(1),
                tabs: vec![TabRecord {
                    id: 1,
                    kind: TabRecordKind::Shell,
                    title: "shell".into(),
                }],
                agent: None,
            },
        );
        let n = migrate_task_pane_keys(&mut state, &[("auth".into(), "jam/auth".into())]);
        assert_eq!(n, 1);
        assert!(!state.worktrees.contains_key("task:auth"));
        assert!(state.worktrees.contains_key("task:auth:member:jam/auth"));
    }

    fn rec(id: u64) -> WorktreePanes {
        WorktreePanes {
            active: Some(id),
            tabs: vec![TabRecord {
                id,
                kind: TabRecordKind::Shell,
                title: "shell".into(),
            }],
            agent: None,
        }
    }

    #[test]
    fn save_keeps_another_instances_tabs_and_counter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        // The other instance saved its task tabs and drew ids up to 343.
        let mut other = PanesState {
            next_tab_id: 343,
            ..Default::default()
        };
        other
            .worktrees
            .insert("task:t:member:@coordinator".into(), rec(338));
        save(&path, &mut other);

        // We loaded long ago: stale counter, no task tabs.
        let mut ours = PanesState {
            next_tab_id: 337,
            ..Default::default()
        };
        ours.worktrees.insert("main:repo".into(), rec(12));
        save(&path, &mut ours);

        let back = load(&path);
        assert_eq!(back.next_tab_id, 343, "counter must never go backwards");
        assert!(back.worktrees.contains_key("task:t:member:@coordinator"));
        assert!(back.worktrees.contains_key("main:repo"));
    }

    #[test]
    fn save_does_not_resurrect_a_key_we_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = PanesState::default();
        state.worktrees.insert("gone".into(), rec(1));
        save(&path, &mut state);

        state.remove("gone");
        save(&path, &mut state);
        assert!(!load(&path).worktrees.contains_key("gone"));
    }

    #[test]
    fn our_records_win_over_disk_for_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut disk = PanesState::default();
        disk.worktrees.insert("k".into(), rec(1));
        save(&path, &mut disk);

        let mut ours = PanesState::default();
        ours.worktrees.insert("k".into(), rec(2));
        save(&path, &mut ours);
        assert_eq!(load(&path).worktrees["k"].active, Some(2));
    }

    #[test]
    fn missing_file_is_default() {
        let got = load(Path::new("/no/such/panes.json"));
        assert_eq!(got, PanesState::default());
        assert_eq!(got.next_tab_id, 0);
    }
}
