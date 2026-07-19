//! Hardcoded worktree/sidebar model for Spike 1. Spike 4 replaces this with
//! the on-disk registry (docs/architecture.md §6.1).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentStatus {
    Running,
    Waiting,
    Idle,
}

impl AgentStatus {
    pub fn label(&self) -> &'static str {
        match self {
            AgentStatus::Running => "Running",
            AgentStatus::Waiting => "Waiting",
            AgentStatus::Idle => "Idle",
        }
    }
}

#[derive(Clone)]
pub struct WorktreeCard {
    pub initials: &'static str,
    pub name: &'static str,
    pub branch: &'static str,
    pub status: AgentStatus,
    pub dirty_files: usize,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Clone)]
pub struct RepoGroup {
    pub name: &'static str,
    pub default_branch: &'static str,
    pub worktrees: Vec<WorktreeCard>,
}

#[derive(Clone)]
pub struct ChangedFile {
    pub path: &'static str,
    pub status: char, // 'M' | 'A' | 'D'
    pub adds: usize,
    pub dels: usize,
}

pub fn mock_repos() -> Vec<RepoGroup> {
    vec![
        RepoGroup {
            name: "omar-harness",
            default_branch: "main",
            worktrees: vec![
                WorktreeCard {
                    initials: "WN",
                    name: "worktree nav",
                    branch: "feat/wt-nav",
                    status: AgentStatus::Running,
                    dirty_files: 4,
                    ahead: 3,
                    behind: 0,
                },
                WorktreeCard {
                    initials: "DP",
                    name: "diff panel",
                    branch: "feat/diff-panel",
                    status: AgentStatus::Waiting,
                    dirty_files: 2,
                    ahead: 1,
                    behind: 2,
                },
            ],
        },
        RepoGroup {
            name: "pi",
            default_branch: "main",
            worktrees: vec![WorktreeCard {
                initials: "HS",
                name: "harness status ext",
                branch: "feat/harness-status",
                status: AgentStatus::Idle,
                dirty_files: 0,
                ahead: 0,
                behind: 0,
            }],
        },
    ]
}

pub fn mock_changed_files() -> Vec<ChangedFile> {
    vec![
        ChangedFile {
            path: "crates/harness-app/src/sidebar.rs",
            status: 'M',
            adds: 24,
            dels: 6,
        },
        ChangedFile {
            path: "crates/harness-app/src/workspace.rs",
            status: 'M',
            adds: 11,
            dels: 2,
        },
        ChangedFile {
            path: "crates/worktrees/src/registry.rs",
            status: 'A',
            adds: 3,
            dels: 0,
        },
    ]
}
