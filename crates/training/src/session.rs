//! Parsing pi session JSONL (architecture §6.4).
//!
//! A session file is an **append-only tree**, not a flat log: every record
//! carries `id` and `parentId`, and `/fork` or `switch_session` creates a
//! branch. Reading the records in file order therefore interleaves abandoned
//! branches with the live one, which would train the model on paths the user
//! explicitly walked away from.
//!
//! So we resolve the **active branch**: take the last record in the file and
//! walk `parentId` back to the root. Because the format is append-only, the
//! final record always belongs to whichever branch was current when the session
//! ended, and its ancestor chain is exactly the conversation that produced the
//! final diff.
//!
//! Verified against a real capture from `@mariozechner/pi-coding-agent@0.73.1`
//! (`fixtures/pi-session-v3.jsonl`, `version: 3`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A parsed session, reduced to its active branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// The session-format version pi wrote (3 at the pinned pi).
    pub format_version: u32,
    pub cwd: PathBuf,
    pub started_at: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    /// The active branch, root-first.
    pub turns: Vec<Turn>,
}

impl Session {
    /// Number of assistant tool calls across the branch — a cheap proxy for
    /// how much work the session actually did.
    pub fn tool_call_count(&self) -> usize {
        self.turns
            .iter()
            .map(|t| match t {
                Turn::Assistant { tool_calls, .. } => tool_calls.len(),
                _ => 0,
            })
            .sum()
    }

    /// Whether any tool result came back as an error.
    pub fn had_tool_errors(&self) -> bool {
        self.turns
            .iter()
            .any(|t| matches!(t, Turn::ToolResult { is_error: true, .. }))
    }
}

/// One conversational step on the active branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Turn {
    User {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Assistant {
        text: Option<String>,
        tool_calls: Vec<ToolCall>,
        stop_reason: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_call_id: String,
        tool_name: String,
        text: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug)]
pub enum SessionError {
    Io(std::io::Error),
    /// No `{"type":"session"}` header — not a pi session file.
    NotASession(PathBuf),
    /// The header declares a format version this build has not been checked
    /// against. Refused rather than guessed at: silently mis-parsing sessions
    /// would poison the training set (§6.4).
    UnsupportedVersion { path: PathBuf, version: u32 },
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionError::Io(e) => write!(f, "{e}"),
            SessionError::NotASession(p) => {
                write!(f, "{} has no session header record", p.display())
            }
            SessionError::UnsupportedVersion { path, version } => write!(
                f,
                "{} declares session format v{version}; this build parses v{}",
                path.display(),
                SUPPORTED_FORMAT_VERSION
            ),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<std::io::Error> for SessionError {
    fn from(e: std::io::Error) -> Self {
        SessionError::Io(e)
    }
}

/// The pi session-format version this build understands.
pub const SUPPORTED_FORMAT_VERSION: u32 = 3;

/// Read and resolve a session file to its active branch.
pub fn read_session(path: &Path) -> Result<Session, SessionError> {
    let text = std::fs::read_to_string(path)?;
    parse_session(&text, path)
}

/// Parse session JSONL. Malformed lines are skipped rather than fatal — a
/// session truncated by a crash is still worth harvesting up to the break.
pub fn parse_session(text: &str, path: &Path) -> Result<Session, SessionError> {
    let records: Vec<serde_json::Value> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let header = records
        .iter()
        .find(|r| r["type"] == "session")
        .ok_or_else(|| SessionError::NotASession(path.to_path_buf()))?;

    let format_version = header["version"].as_u64().unwrap_or(0) as u32;
    if format_version != SUPPORTED_FORMAT_VERSION {
        return Err(SessionError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: format_version,
        });
    }

    // Latest model_change on record wins — that is the model that produced the
    // final state of the session.
    let model_change = records.iter().rev().find(|r| r["type"] == "model_change");

    let by_id: HashMap<&str, &serde_json::Value> = records
        .iter()
        .filter_map(|r| r["id"].as_str().map(|id| (id, r)))
        .collect();

    // Walk back from the final record: on an append-only tree that is always
    // the live branch's leaf.
    let mut chain: Vec<&serde_json::Value> = Vec::new();
    // Defensive: a corrupt file could contain a parent cycle. Stopping the
    // first time an id repeats both terminates and avoids splicing the same
    // turns in twice, which a simple iteration cap would not.
    let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut cursor = records.iter().rev().find(|r| r["id"].is_string());
    while let Some(record) = cursor {
        let Some(id) = record["id"].as_str() else { break };
        if !visited.insert(id) {
            break;
        }
        chain.push(record);
        cursor = record["parentId"]
            .as_str()
            .and_then(|parent| by_id.get(parent).copied());
    }
    chain.reverse();

    let turns = chain.iter().filter_map(|r| turn_from(r)).collect();

    Ok(Session {
        id: header["id"].as_str().unwrap_or_default().to_string(),
        format_version,
        cwd: PathBuf::from(header["cwd"].as_str().unwrap_or_default()),
        started_at: header["timestamp"].as_str().unwrap_or_default().to_string(),
        provider: model_change
            .and_then(|m| m["provider"].as_str())
            .map(str::to_string),
        model: model_change
            .and_then(|m| m["modelId"].as_str())
            .map(str::to_string),
        turns,
    })
}

/// Convert one `{"type":"message"}` record into a [`Turn`].
fn turn_from(record: &serde_json::Value) -> Option<Turn> {
    if record["type"] != "message" {
        return None;
    }
    let message = &record["message"];
    match message["role"].as_str()? {
        "user" => Some(Turn::User {
            text: concat_text(&message["content"]),
        }),
        "assistant" => {
            let text = concat_text(&message["content"]);
            let tool_calls = message["content"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter(|p| p["type"] == "toolCall")
                        .map(|p| ToolCall {
                            id: p["id"].as_str().unwrap_or_default().to_string(),
                            name: p["name"].as_str().unwrap_or_default().to_string(),
                            arguments: p["arguments"].clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(Turn::Assistant {
                text: (!text.is_empty()).then_some(text),
                tool_calls,
                stop_reason: message["stopReason"].as_str().map(str::to_string),
            })
        }
        "toolResult" => Some(Turn::ToolResult {
            tool_call_id: message["toolCallId"].as_str().unwrap_or_default().to_string(),
            tool_name: message["toolName"].as_str().unwrap_or_default().to_string(),
            text: concat_text(&message["content"]),
            is_error: message["isError"].as_bool().unwrap_or(false),
        }),
        _ => None,
    }
}

/// Join every `{"type":"text"}` part of a content array.
///
/// Non-text parts (images, tool calls) are skipped here by design: the caller
/// handles tool calls separately, and image bytes have no place in a text
/// training record.
fn concat_text(content: &serde_json::Value) -> String {
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter(|p| p["type"] == "text")
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}
