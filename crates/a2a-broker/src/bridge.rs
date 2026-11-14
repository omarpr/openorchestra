//! Bridge from A2A SendMessage into a member agent session.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
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
    /// Byte offset of the first outbox line not yet consumed.
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
        // Start at the end. The task store is in memory, so a completion
        // written before this broker started names a task it has never seen;
        // replaying the whole file on every restart only re-ran those no-ops
        // (and re-appended them to the audit log).
        let start = fs::metadata(&outbox).map(|m| m.len()).unwrap_or(0);
        Ok(FileBridge {
            root,
            outbox_cursor: Mutex::new(start),
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
        let mut cursor = self.outbox_cursor.lock().ok()?;
        loop {
            // Read only from the cursor on: the drain loop polls this every
            // 200ms, and re-reading the whole append-only file each time made
            // every poll cost grow with the task's history.
            let mut f = File::open(self.outbox_path()).ok()?;
            let len = f.metadata().ok()?.len();
            if len < *cursor {
                // Truncated or replaced underneath us: start over.
                *cursor = 0;
            }
            f.seek(SeekFrom::Start(*cursor)).ok()?;
            let mut buf = Vec::new();
            BufReader::new(f).read_until(b'\n', &mut buf).ok()?;
            // Only a complete line counts; a half-written one waits for its
            // newline.
            if buf.last() != Some(&b'\n') {
                return None;
            }
            *cursor += buf.len() as u64;
            if let Some(ev) = parse_completion(&String::from_utf8_lossy(&buf)) {
                return Some(ev);
            }
            // A blank or malformed line: skip it rather than stall the queue.
        }
    }
}

fn parse_completion(line: &str) -> Option<BridgeEvent> {
    {
        let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
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
