//! Persisted record of which tabs were open per worktree, so terminal/agent
//! tabs can be reattached to their tmux sessions on reopen. Same load/save
//! shape as prefs/repos (a JSON file under the app state dir).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

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
    /// restart). A task pane with this set and no live agent shows STOPPED
    /// with a Restart. Cleared only when the user closes the agent tab.
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
}

pub fn load(path: &Path) -> PanesState {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PanesState::default(),
    }
}

/// Rewrite legacy `task:{id}` pane keys to `task:{id}:member:{home_worktree_id}`.
///
/// Returns the number of keys migrated. Idempotent.
pub fn migrate_task_pane_keys(
    state: &mut PanesState,
    home_by_task: &[(String, String)],
) -> usize {
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

/// Merge with the file on disk (see [`PanesState::merge_from`]), then write.
pub fn save(path: &Path, state: &mut PanesState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    state.merge_from(load(path));
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = crate::state_files::write_private(path, json.as_bytes()) {
                log::warn!("could not write panes state to {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("could not serialize panes state: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");

        let mut state = PanesState { next_tab_id: 5, ..Default::default() };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(3),
                tabs: vec![
                    TabRecord { id: 3, kind: TabRecordKind::Agent(AgentKind::Pi), title: "pi".into() },
                    TabRecord { id: 4, kind: TabRecordKind::Shell, title: "shell".into() },
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
        let mut state = PanesState { next_tab_id: 2, ..Default::default() };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(1),
                tabs: vec![TabRecord { id: 1, kind: TabRecordKind::Diff, title: "⑂ diff".into() }],
                agent: None,
            },
        );
        save(&path, &mut state);
        assert_eq!(load(&path), state);
    }

    #[test]
    fn migrate_task_pane_keys_rewrites_legacy() {
        let mut state = PanesState { next_tab_id: 2, ..Default::default() };
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
        let n = migrate_task_pane_keys(
            &mut state,
            &[("auth".into(), "jam/auth".into())],
        );
        assert_eq!(n, 1);
        assert!(!state.worktrees.contains_key("task:auth"));
        assert!(state
            .worktrees
            .contains_key("task:auth:member:jam/auth"));
    }

    fn rec(id: u64) -> WorktreePanes {
        WorktreePanes {
            active: Some(id),
            tabs: vec![TabRecord { id, kind: TabRecordKind::Shell, title: "shell".into() }],
            agent: None,
        }
    }

    #[test]
    fn save_keeps_another_instances_tabs_and_counter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        // The other instance saved its task tabs and drew ids up to 343.
        let mut other = PanesState { next_tab_id: 343, ..Default::default() };
        other.worktrees.insert("task:t:member:@coordinator".into(), rec(338));
        save(&path, &mut other);

        // We loaded long ago: stale counter, no task tabs.
        let mut ours = PanesState { next_tab_id: 337, ..Default::default() };
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
