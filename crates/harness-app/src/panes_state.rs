//! Persisted record of which tabs were open per worktree, so terminal/agent
//! tabs can be reattached to their tmux sessions on reopen. Same load/save
//! shape as prefs/repos (a JSON file under the app state dir).

use std::collections::BTreeMap;
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
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorktreePanes {
    #[serde(default)]
    pub active: Option<u64>,
    #[serde(default)]
    pub tabs: Vec<TabRecord>,
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
        if let Some(wp) = state.worktrees.remove(&legacy) {
            let neu = a2a_broker::task_member_pane_key(task_id, home_wt);
            // Prefer existing new-key data if both somehow exist.
            state.worktrees.entry(neu).or_insert(wp);
            moved += 1;
        }
    }
    moved
}

pub fn save(path: &Path, state: &PanesState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match serde_json::to_string_pretty(state) {
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
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

        let mut state = PanesState { next_tab_id: 5, worktrees: BTreeMap::new() };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(3),
                tabs: vec![
                    TabRecord { id: 3, kind: TabRecordKind::Agent(AgentKind::Pi), title: "pi".into() },
                    TabRecord { id: 4, kind: TabRecordKind::Shell, title: "shell".into() },
                ],
            },
        );

        save(&path, &state);
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
        let mut state = PanesState { next_tab_id: 2, worktrees: BTreeMap::new() };
        state.worktrees.insert(
            "acme/fix".into(),
            WorktreePanes {
                active: Some(1),
                tabs: vec![TabRecord { id: 1, kind: TabRecordKind::Diff, title: "⑂ diff".into() }],
            },
        );
        save(&path, &state);
        assert_eq!(load(&path), state);
    }

    #[test]
    fn migrate_task_pane_keys_rewrites_legacy() {
        let mut state = PanesState {
            next_tab_id: 2,
            worktrees: BTreeMap::new(),
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

    #[test]
    fn missing_file_is_default() {
        let got = load(Path::new("/no/such/panes.json"));
        assert_eq!(got, PanesState::default());
        assert_eq!(got.next_tab_id, 0);
    }
}
