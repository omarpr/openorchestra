//! Audit JSONL for A2A messages and task transitions.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

#[derive(Debug)]
pub struct AuditLog {
    path: PathBuf,
    file: File,
}

impl AuditLog {
    pub fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(AuditLog { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn append<T: Serialize>(&mut self, event: &T) -> io::Result<()> {
        serde_json::to_writer(&mut self.file, event)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        self.file.write_all(b"\n")?;
        self.file.flush()?;
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct AuditEvent<'a> {
    pub ts: &'a str,
    pub kind: &'a str,
    pub oo_task_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub a2a_task_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<serde_json::Value>,
}
