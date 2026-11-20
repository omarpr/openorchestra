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
    /// Persisted only in `browser-tabs.json`, never in `panes.json` or
    /// `review-tabs.json`. See [`SIDECARS`].
    Browser {
        url: String,
        #[serde(default)]
        title: Option<String>,
    },
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

/// Tab kinds an older build cannot deserialize live in sidecar files beside
/// `panes.json`. An installed build and a dev build share `panes.json`; a tab
/// kind the older build cannot deserialize makes `load` return an empty state,
/// and that build's next save then drops the other instance's tabs and the
/// tab-id counter. Each kind belongs to exactly one sidecar, because the
/// release build also reads `review-tabs.json` and would drop its review tabs
/// on meeting a kind it does not know.
struct Sidecar {
    file: &'static str,
    claims: fn(&TabRecordKind) -> bool,
}

const REVIEW_TABS_FILE: &str = "review-tabs.json";
const BROWSER_TABS_FILE: &str = "browser-tabs.json";

const SIDECARS: [Sidecar; 2] = [
    Sidecar {
        file: REVIEW_TABS_FILE,
        claims: |kind| matches!(kind, TabRecordKind::ReviewOverview(_)),
    },
    Sidecar {
        file: BROWSER_TABS_FILE,
        claims: |kind| matches!(kind, TabRecordKind::Browser { .. }),
    },
];

#[derive(Debug, Default, Serialize, Deserialize)]
struct SidecarFile {
    #[serde(default)]
    worktrees: BTreeMap<String, SidecarWorktreeTabs>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct SidecarWorktreeTabs {
    /// Set when the active tab belongs to this sidecar. Absent when a legacy
    /// tab (or another sidecar's tab) is active, so a reload does not override
    /// that choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    active: Option<u64>,
    tabs: Vec<PlacedTab>,
}

/// A sidecar tab and its position in the worktree's full tab list.
#[derive(Debug, Serialize, Deserialize)]
struct PlacedTab {
    index: usize,
    record: TabRecord,
}

fn split_sidecars(state: &PanesState) -> (BTreeMap<String, WorktreePanes>, [SidecarFile; 2]) {
    let mut legacy = BTreeMap::new();
    let mut files: [SidecarFile; 2] = Default::default();
    for (key, wp) in &state.worktrees {
        let mut tabs = Vec::new();
        let mut placed: [Vec<PlacedTab>; 2] = Default::default();
        for (index, tab) in wp.tabs.iter().enumerate() {
            match SIDECARS.iter().position(|s| (s.claims)(&tab.kind)) {
                Some(owner) => placed[owner].push(PlacedTab {
                    index,
                    record: tab.clone(),
                }),
                None => tabs.push(tab.clone()),
            }
        }
        // The sidecar owning the active tab records it. The shared file gets
        // a tab it actually holds: an active id the older build's tab list
        // does not contain is how a later save of that build loses track of
        // which tab was open.
        let owner = wp.active.and_then(|id| {
            placed
                .iter()
                .position(|p| p.iter().any(|t| t.record.id == id))
        });
        let active = if owner.is_some() {
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
        for (i, tabs) in placed.into_iter().enumerate() {
            if !tabs.is_empty() {
                files[i].worktrees.insert(
                    key.clone(),
                    SidecarWorktreeTabs {
                        active: if owner == Some(i) { wp.active } else { None },
                        tabs,
                    },
                );
            }
        }
    }
    (legacy, files)
}

/// Put every sidecar tab back at its recorded position. All sidecars are read
/// first and their tabs inserted in ascending index order, so the original
/// order comes back whichever file a tab came from.
fn splice_sidecars(state: &mut PanesState, dir: &Path) {
    let mut placed: BTreeMap<String, Vec<PlacedTab>> = BTreeMap::new();
    let mut active: BTreeMap<String, u64> = BTreeMap::new();
    for sidecar in &SIDECARS {
        let Ok(text) = std::fs::read_to_string(dir.join(sidecar.file)) else {
            continue;
        };
        let Ok(file) = serde_json::from_str::<SidecarFile>(&text) else {
            continue;
        };
        for (key, tabs) in file.worktrees {
            if let Some(id) = tabs.active {
                active.insert(key.clone(), id);
            }
            placed.entry(key).or_default().extend(tabs.tabs);
        }
    }
    for (key, mut tabs) in placed {
        let wp = state.worktrees.entry(key.clone()).or_default();
        tabs.sort_by_key(|tab| tab.index);
        for tab in tabs {
            if wp.tabs.iter().any(|existing| existing.id == tab.record.id) {
                continue;
            }
            let at = tab.index.min(wp.tabs.len());
            wp.tabs.insert(at, tab.record);
        }
        if let Some(id) = active.get(&key)
            && wp.tabs.iter().any(|tab| tab.id == *id)
        {
            wp.active = Some(*id);
        }
    }
}

pub fn load(path: &Path) -> PanesState {
    let mut state = match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PanesState::default(),
    };
    splice_sidecars(&mut state, path.parent().unwrap_or_else(|| Path::new("")));
    state
}

/// Merge with the file on disk (see [`PanesState::merge_from`]), then write.
///
/// Review overview and browser tabs are not part of the shared file. See
/// [`Sidecar`].
pub fn save(path: &Path, state: &mut PanesState) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    state.merge_from(load(path));
    let (worktrees, files) = split_sidecars(state);
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
    for (sidecar, file) in SIDECARS.iter().zip(&files) {
        let sidecar_path = path.with_file_name(sidecar.file);
        if file.worktrees.is_empty() {
            let _ = std::fs::remove_file(&sidecar_path);
            continue;
        }
        match serde_json::to_string_pretty(file) {
            Ok(json) => {
                if let Err(e) = crate::state_files::write_private(&sidecar_path, json.as_bytes()) {
                    log::warn!(
                        "could not write {} to {}: {e}",
                        sidecar.file,
                        sidecar_path.display()
                    );
                }
            }
            Err(e) => log::warn!("could not serialize {}: {e}", sidecar.file),
        }
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

    fn shell(id: u64) -> TabRecord {
        TabRecord {
            id,
            kind: TabRecordKind::Shell,
            title: "shell".into(),
        }
    }

    fn browser(id: u64) -> TabRecord {
        TabRecord {
            id,
            kind: TabRecordKind::Browser {
                url: format!("https://example.com/{id}"),
                title: Some(format!("Page {id}")),
            },
            title: "Browser".into(),
        }
    }

    fn review(id: u64) -> TabRecord {
        TabRecord {
            id,
            kind: TabRecordKind::ReviewOverview(None),
            title: "Review Overview".into(),
        }
    }

    fn one_worktree(active: Option<u64>, tabs: Vec<TabRecord>) -> PanesState {
        let mut state = PanesState {
            next_tab_id: 20,
            ..Default::default()
        };
        state.worktrees.insert(
            "main:project".into(),
            WorktreePanes {
                active,
                tabs,
                agent: None,
            },
        );
        state
    }

    #[test]
    fn browser_tabs_stay_out_of_panes_and_review_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = one_worktree(Some(3), vec![shell(1), review(2), browser(3)]);
        save(&path, &mut state);

        let panes = std::fs::read_to_string(&path).unwrap();
        assert!(!panes.contains("Browser"), "{panes}");
        assert!(!panes.contains("ReviewOverview"), "{panes}");
        let reviews = std::fs::read_to_string(dir.path().join("review-tabs.json")).unwrap();
        assert!(!reviews.contains("Browser"), "{reviews}");
        assert!(reviews.contains("ReviewOverview"), "{reviews}");
        let browsers = std::fs::read_to_string(dir.path().join("browser-tabs.json")).unwrap();
        assert!(browsers.contains("https://example.com/3"), "{browsers}");
        assert!(!browsers.contains("ReviewOverview"), "{browsers}");
        // The shared file's active id names a tab it actually holds.
        let legacy: PanesState = serde_json::from_str(&panes).unwrap();
        assert_eq!(legacy.worktrees["main:project"].active, Some(1));
    }

    #[test]
    fn browser_tabs_restore_in_place_and_keep_active() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = one_worktree(Some(2), vec![shell(1), browser(2), shell(3)]);
        save(&path, &mut state);
        let back = load(&path);
        assert_eq!(back, state);
        let wp = &back.worktrees["main:project"];
        assert_eq!(wp.active, Some(2));
        assert!(matches!(
            &wp.tabs[1].kind,
            TabRecordKind::Browser { title: Some(t), .. } if t == "Page 2"
        ));
    }

    #[test]
    fn interleaved_review_and_browser_tabs_restore_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = one_worktree(
            Some(5),
            vec![shell(1), browser(2), review(3), shell(4), browser(5)],
        );
        save(&path, &mut state);
        assert_eq!(load(&path), state);

        // Only one sidecar claims the active tab.
        let reviews = std::fs::read_to_string(dir.path().join("review-tabs.json")).unwrap();
        assert!(!reviews.contains("\"active\""), "{reviews}");
    }

    #[test]
    fn an_older_build_save_keeps_browser_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let mut state = one_worktree(Some(2), vec![shell(1), browser(2)]);
        save(&path, &mut state);

        // An older build rewrites panes.json with only what it understands.
        let older = PanesState {
            next_tab_id: 25,
            ..one_worktree(Some(1), vec![shell(1)])
        };
        std::fs::write(&path, serde_json::to_string_pretty(&older).unwrap()).unwrap();

        let back = load(&path);
        assert_eq!(back.next_tab_id, 25);
        let wp = &back.worktrees["main:project"];
        assert_eq!(wp.tabs, vec![shell(1), browser(2)]);
        assert_eq!(wp.active, Some(2));
    }

    #[test]
    fn no_browser_tabs_removes_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panes.json");
        let browser_path = dir.path().join("browser-tabs.json");
        let mut state = one_worktree(Some(2), vec![shell(1), browser(2)]);
        save(&path, &mut state);
        assert!(browser_path.exists());

        let wp = state.worktrees.get_mut("main:project").unwrap();
        wp.tabs.pop();
        wp.active = Some(1);
        save(&path, &mut state);
        assert!(!browser_path.exists());
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
