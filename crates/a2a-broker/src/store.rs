//! In-memory A2A task store keyed by task id.

use std::collections::HashMap;

use crate::protocol::{Artifact, Message, Task, TaskState, TaskStatus};

#[derive(Debug, Default)]
pub struct TaskStore {
    tasks: HashMap<String, Task>,
}

impl TaskStore {
    pub fn insert(&mut self, task: Task) {
        self.tasks.insert(task.id.clone(), task);
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
