//! In-memory A2A task store keyed by task id.

use std::collections::{HashMap, VecDeque};

use crate::protocol::{Artifact, Message, Task, TaskState, TaskStatus};

/// Most tasks kept. Past this, the oldest *finished* tasks are dropped;
/// live ones never are, so a long session can't grow this without bound.
pub const MAX_TASKS: usize = 500;

#[derive(Debug, Default)]
pub struct TaskStore {
    tasks: HashMap<String, Task>,
    /// Insertion order, oldest first, for eviction.
    order: VecDeque<String>,
}

fn finished(state: TaskState) -> bool {
    matches!(
        state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Rejected
    )
}

impl TaskStore {
    pub fn insert(&mut self, task: Task) {
        let id = task.id.clone();
        if self.tasks.insert(id.clone(), task).is_none() {
            self.order.push_back(id);
        }
        self.evict();
    }

    fn evict(&mut self) {
        let mut ix = 0;
        while self.tasks.len() > MAX_TASKS && ix < self.order.len() {
            let done = self.tasks.get(&self.order[ix]).is_none_or(|t| finished(t.status.state));
            if done {
                if let Some(id) = self.order.remove(ix) {
                    self.tasks.remove(&id);
                }
            } else {
                ix += 1;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&Task> {
        self.tasks.get(id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.tasks.get_mut(id)
    }

    pub fn list_for_context(&self, context_id: &str) -> Vec<&Task> {
        let mut out: Vec<_> = self
            .tasks
            .values()
            .filter(|t| t.context_id == context_id)
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn set_state(
        &mut self,
        id: &str,
        state: TaskState,
        message: Option<Message>,
        timestamp: String,
    ) -> Option<&Task> {
        let task = self.tasks.get_mut(id)?;
        task.status = TaskStatus {
            state,
            message,
            timestamp: Some(timestamp),
        };
        Some(task)
    }

    pub fn add_artifact(&mut self, id: &str, artifact: Artifact) -> Option<&Task> {
        let task = self.tasks.get_mut(id)?;
        task.artifacts.push(artifact);
        Some(task)
    }

    pub fn cancel(&mut self, id: &str, timestamp: String) -> Option<&Task> {
        self.set_state(id, TaskState::Canceled, None, timestamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, state: TaskState) -> Task {
        Task {
            id: id.into(),
            context_id: "c".into(),
            tenant: "t".into(),
            status: TaskStatus { state, message: None, timestamp: None },
            history: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    #[test]
    fn finished_tasks_are_evicted_oldest_first_and_live_ones_never() {
        let mut s = TaskStore::default();
        s.insert(task("live", TaskState::Working));
        for i in 0..MAX_TASKS + 10 {
            s.insert(task(&format!("t{i}"), TaskState::Completed));
        }
        assert_eq!(s.len(), MAX_TASKS);
        assert!(s.get("live").is_some(), "a working task is never evicted");
        assert!(s.get("t0").is_none(), "the oldest finished task goes first");
        assert!(s.get(&format!("t{}", MAX_TASKS + 9)).is_some());
    }
}
