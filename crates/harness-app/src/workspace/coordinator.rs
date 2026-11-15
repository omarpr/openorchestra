//! Which context a task selection lands on: its coordinator agent or one of
//! its repo members.
//!
//! A task with 2+ members runs a dedicated coordinator agent in the task
//! folder (no repo of its own). Every repo is a specialist, the former home
//! included. A single-member task has no coordinator: its one member is the
//! task. Pure so the rule is testable without a window.

/// What a task selection focuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TaskFocus<'a> {
    /// The task's coordinator agent (cwd = the task folder).
    Coordinator,
    /// One repo member's worktree.
    Member(&'a worktrees::TaskMember),
}

/// Whether this task runs a coordinator agent.
pub(crate) fn has_coordinator(task: &worktrees::Task) -> bool {
    task.members.len() >= 2
}

/// Resolve a task selection to its focus.
///
/// `member_id` names a member worktree; `None` is the task itself, which is
/// the coordinator when there is one and the sole member otherwise. An
/// unknown member is `None`.
pub(crate) fn task_focus<'a>(
    task: &'a worktrees::Task,
    member_id: Option<&str>,
) -> Option<TaskFocus<'a>> {
    match member_id {
        Some(id) => task
            .members
            .iter()
            .find(|m| m.worktree_id == id)
            .map(TaskFocus::Member),
        None if has_coordinator(task) => Some(TaskFocus::Coordinator),
        None => task.home_member().map(TaskFocus::Member),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn member(repo: &str, id: &str) -> worktrees::TaskMember {
        worktrees::TaskMember {
            repo: repo.into(),
            worktree_id: id.into(),
            path: PathBuf::from(format!("/wt/{id}")),
            branch: format!("oo/{id}"),
        }
    }

    fn task(members: Vec<worktrees::TaskMember>) -> worktrees::Task {
        worktrees::Task {
            id: "t1".into(),
            name: "Ship it".into(),
            slug: "ship-it".into(),
            home_repo: members.first().map(|m| m.repo.clone()).unwrap_or_default(),
            members,
            agent_kind: Default::default(),
            created_at: "2026-11-15T00:00:00Z".into(),
            archived: false,
        }
    }

    fn two() -> worktrees::Task {
        task(vec![member("api", "wt-api"), member("web", "wt-web")])
    }

    #[test]
    fn two_member_task_with_no_member_focuses_coordinator() {
        let t = two();
        assert_eq!(task_focus(&t, None), Some(TaskFocus::Coordinator));
        assert!(has_coordinator(&t));
    }

    #[test]
    fn restored_none_selection_is_coordinator() {
        // A persisted `Selection::Task { member_id: None }` from before the
        // coordinator existed meant "home"; restored now it means the
        // coordinator, never the old home member.
        let t = two();
        assert_eq!(task_focus(&t, None), Some(TaskFocus::Coordinator));
    }

    #[test]
    fn naming_the_former_home_focuses_that_member() {
        let t = two();
        let home = t.home_member().unwrap();
        assert_eq!(
            task_focus(&t, Some("wt-api")),
            Some(TaskFocus::Member(home))
        );
    }

    #[test]
    fn single_member_task_focuses_its_member() {
        let t = task(vec![member("api", "wt-api")]);
        assert!(!has_coordinator(&t));
        assert_eq!(
            task_focus(&t, None),
            Some(TaskFocus::Member(&t.members[0]))
        );
    }

    #[test]
    fn unknown_member_is_none() {
        let t = two();
        assert_eq!(task_focus(&t, Some("wt-nope")), None);
    }
}
