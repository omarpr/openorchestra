//! Bridge from A2A SendMessage into a member agent session.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// One delegation delivered to a specialist (or orchestrator) inbox.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InboxMessage {
    pub a2a_task_id: String,
    pub context_id: String,
    pub from_tenant: Option<String>,
    pub text: String,
    pub created_at: String,
}

/// Harness-facing notification that a message was enqueued for a tenant.
#[derive(Debug, Clone)]
pub struct InboxDelivery {
    pub oo_task_id: String,
    pub tenant: String,
    pub message: InboxMessage,
}

#[derive(Debug, Clone)]
pub enum BridgeEvent {
    /// Specialist (or harness) completed a delegated A2A task.
    Completed {
        a2a_task_id: String,
        summary: String,
    },
    Failed {
        a2a_task_id: String,
        error: String,
    },
}

/// How the broker hands work to opaque coding-agent CLIs.
pub trait Bridge: Send + Sync {
    fn enqueue(&self, tenant: &str, msg: &InboxMessage) -> io::Result<()>;
    fn try_recv_completion(&self) -> Option<BridgeEvent>;
}

/// File-based bridge: `inbox/<tenant>.jsonl` + `outbox.jsonl`.
pub struct FileBridge {
    root: PathBuf,
    outbox_cursor: Mutex<u64>,
}

impl FileBridge {
    pub fn new(a2a_dir: impl Into<PathBuf>) -> io::Result<Self> {
        let root = a2a_dir.into();
        fs::create_dir_all(root.join("inbox"))?;
        let outbox = root.join("outbox.jsonl");
        if !outbox.exists() {
            File::create(&outbox)?;
        }
        Ok(FileBridge {
            root,
            outbox_cursor: Mutex::new(0),
        })
    }

    pub fn inbox_path(&self, tenant: &str) -> PathBuf {
        // Sanitize path separators in worktree ids (repo/slug).
        let safe = tenant.replace('/', "__");
        self.root.join("inbox").join(format!("{safe}.jsonl"))
    }

    pub fn outbox_path(&self) -> PathBuf {
        self.root.join("outbox.jsonl")
    }

    /// Specialist / harness helper: append a completion line to the outbox.
    pub fn write_completion(a2a_dir: &Path, event: &BridgeEvent) -> io::Result<()> {
        fs::create_dir_all(a2a_dir)?;
        let path = a2a_dir.join("outbox.jsonl");
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        let line = match event {
            BridgeEvent::Completed {
                a2a_task_id,
                summary,
            } => serde_json::json!({
                "kind": "completed",
                "a2a_task_id": a2a_task_id,
                "summary": summary,
            }),
            BridgeEvent::Failed { a2a_task_id, error } => serde_json::json!({
                "kind": "failed",
                "a2a_task_id": a2a_task_id,
                "error": error,
            }),
        };
        writeln!(f, "{}", serde_json::to_string(&line).unwrap())?;
        Ok(())
    }
}

impl Bridge for FileBridge {
    fn enqueue(&self, tenant: &str, msg: &InboxMessage) -> io::Result<()> {
        let path = self.inbox_path(tenant);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{}", serde_json::to_string(msg).unwrap())?;
        Ok(())
    }

    fn try_recv_completion(&self) -> Option<BridgeEvent> {
        let path = self.outbox_path();
        let text = fs::read_to_string(&path).ok()?;
        let mut cursor = self.outbox_cursor.lock().ok()?;
        let bytes = text.as_bytes();
        if (*cursor as usize) >= bytes.len() {
            return None;
        }
        let remaining = &text[*cursor as usize..];
        let (line, rest) = match remaining.split_once('\n') {
            Some((l, r)) => (l, r),
            None => return None,
        };
        *cursor += (line.len() + 1) as u64;
        let _ = rest;
        let v: serde_json::Value = serde_json::from_str(line).ok()?;
        let kind = v.get("kind")?.as_str()?;
        let id = v.get("a2a_task_id")?.as_str()?.to_string();
        match kind {
            "completed" => Some(BridgeEvent::Completed {
                a2a_task_id: id,
                summary: v
                    .get("summary")
                    .and_then(|s| s.as_str())
                    .unwrap_or("")
                    .to_string(),
            }),
            "failed" => Some(BridgeEvent::Failed {
                a2a_task_id: id,
                error: v
                    .get("error")
                    .and_then(|s| s.as_str())
                    .unwrap_or("failed")
                    .to_string(),
            }),
            _ => None,
        }
    }
}
