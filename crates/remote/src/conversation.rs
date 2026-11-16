//! Transcript readers: Claude Code JSONL and pi session JSONL -> display entries.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::Path;

const TEXT_CAP: usize = 64 * 1024;
const THINKING_CAP: usize = 4096;
const RESULT_CAP: usize = 64 * 1024;
const SUMMARY_CAP: usize = 80;
const MARKER: &str = "…[truncated]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    ClaudeCode,
    Pi,
}

impl TranscriptKind {
    pub fn from_agent_kind(kind: &str) -> Option<TranscriptKind> {
        match kind {
            "claude-code" => Some(TranscriptKind::ClaudeCode),
            "pi" => Some(TranscriptKind::Pi),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    User,
    Assistant,
    Thinking,
    Tool,
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolStatus {
    Pending,
    Ok,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLine {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub status: ToolStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<ToolLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    pub truncated: bool,
}

/// Cut `s` to at most `max` bytes on a char boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn capped(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}{MARKER}", cut(s, max))
    }
}

fn parse_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l.trim()).ok())
        .filter(Value::is_object)
        .collect()
}

/// Records on the active branch, oldest first. `start_ok` picks branch tips;
/// the last such record is the tip. Parents are followed through every record.
fn active_branch<'a>(
    records: &'a [Value],
    id_key: &str,
    parent_key: &str,
    start_ok: impl Fn(&Value) -> bool,
) -> Vec<&'a Value> {
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    for r in records {
        if let Some(id) = r[id_key].as_str() {
            by_id.insert(id, r);
        }
    }
    let Some(tip) = records.iter().rev().find(|r| start_ok(r)) else {
        return Vec::new();
    };
    let mut out = vec![tip];
    let mut seen: HashSet<&str> = HashSet::new();
    let mut cur = tip;
    while let Some(parent) = cur[parent_key].as_str() {
        if !seen.insert(parent) {
            break;
        }
        match by_id.get(parent) {
            Some(p) => {
                out.push(p);
                cur = p;
            }
            None => break,
        }
    }
    out.reverse();
    out
}

pub fn parse_entries(kind: TranscriptKind, text: &str, cwd: Option<&Path>) -> Vec<Entry> {
    let records = parse_lines(text);
    match kind {
        TranscriptKind::ClaudeCode => claude_entries(&records, cwd),
        TranscriptKind::Pi => pi_entries(&records, cwd),
    }
}

fn text_entry(id: String, kind: EntryKind, text: &str, at: Option<String>) -> Option<Entry> {
    if text.trim().is_empty() {
        return None;
    }
    Some(Entry {
        id,
        kind,
        text: Some(capped(text, TEXT_CAP)),
        tool: None,
        duration_ms: None,
        at,
    })
}

fn notice_entry(id: String, at: Option<String>) -> Entry {
    Entry {
        id,
        kind: EntryKind::Notice,
        text: Some("[image]".to_string()),
        tool: None,
        duration_ms: None,
        at,
    }
}

fn thinking_entry(
    id: String,
    thinking: &str,
    duration_ms: Option<u64>,
    at: Option<String>,
) -> Entry {
    let text = if thinking.trim().is_empty() {
        None
    } else {
        Some(capped(thinking, THINKING_CAP))
    };
    Entry {
        id,
        kind: EntryKind::Thinking,
        text,
        tool: None,
        duration_ms,
        at,
    }
}

fn tool_entry(id: String, tool: ToolLine, at: Option<String>) -> Entry {
    Entry {
        id,
        kind: EntryKind::Tool,
        text: None,
        tool: Some(tool),
        duration_ms: None,
        at,
    }
}

fn status_of(results: &HashMap<String, bool>, tool_id: &str) -> ToolStatus {
    match results.get(tool_id) {
        None => ToolStatus::Pending,
        Some(true) => ToolStatus::Error,
        Some(false) => ToolStatus::Ok,
    }
}

fn claude_blocks(record: &Value) -> Vec<Value> {
    match &record["message"]["content"] {
        Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn claude_entries(records: &[Value], cwd: Option<&Path>) -> Vec<Entry> {
    let mut results: HashMap<String, bool> = HashMap::new();
    for r in records {
        if r["type"] == "user" {
            for b in claude_blocks(r) {
                if b["type"] == "tool_result" {
                    if let Some(id) = b["tool_use_id"].as_str() {
                        results.insert(id.to_string(), b["is_error"].as_bool().unwrap_or(false));
                    }
                }
            }
        }
    }
    let branch = active_branch(records, "uuid", "parentUuid", |r| {
        (r["type"] == "user" || r["type"] == "assistant") && r["isSidechain"] != true
    });
    let mut out = Vec::new();
    for r in branch {
        let role = r["type"].as_str().unwrap_or_default();
        if (role != "user" && role != "assistant")
            || r["isSidechain"] == true
            || r["isMeta"] == true
        {
            continue;
        }
        let uuid = r["uuid"].as_str().unwrap_or_default();
        let at = r["timestamp"].as_str().map(str::to_string);
        for (i, b) in claude_blocks(r).iter().enumerate() {
            let id = format!("{uuid}:{i}");
            let entry = match b["type"].as_str().unwrap_or_default() {
                "text" => {
                    let kind = if role == "user" {
                        EntryKind::User
                    } else {
                        EntryKind::Assistant
                    };
                    text_entry(id, kind, b["text"].as_str().unwrap_or_default(), at.clone())
                }
                "thinking" if role == "assistant" => Some(thinking_entry(
                    id,
                    b["thinking"].as_str().unwrap_or_default(),
                    r["thinkingDurationMs"].as_u64(),
                    at.clone(),
                )),
                "tool_use" if role == "assistant" => {
                    let tid = b["id"].as_str().unwrap_or_default();
                    Some(tool_entry(
                        id,
                        ToolLine {
                            id: tid.to_string(),
                            name: b["name"].as_str().unwrap_or_default().to_string(),
                            summary: tool_summary(
                                b["name"].as_str().unwrap_or_default(),
                                &b["input"],
                                cwd,
                            ),
                            status: status_of(&results, tid),
                        },
                        at.clone(),
                    ))
                }
                "image" => Some(notice_entry(id, at.clone())),
                _ => None,
            };
            out.extend(entry);
        }
    }
    out
}

fn pi_blocks(message: &Value) -> Vec<Value> {
    match &message["content"] {
        Value::String(s) => vec![serde_json::json!({"type": "text", "text": s})],
        Value::Array(a) => a.clone(),
        _ => Vec::new(),
    }
}

fn pi_entries(records: &[Value], cwd: Option<&Path>) -> Vec<Entry> {
    let mut results: HashMap<String, bool> = HashMap::new();
    for r in records {
        if r["type"] == "message" && r["message"]["role"] == "toolResult" {
            if let Some(id) = r["message"]["toolCallId"].as_str() {
                results.insert(
                    id.to_string(),
                    r["message"]["isError"].as_bool().unwrap_or(false),
                );
            }
        }
    }
    let branch = active_branch(records, "id", "parentId", |r| r["type"] == "message");
    let mut out = Vec::new();
    for r in branch {
        if r["type"] != "message" {
            continue;
        }
        let role = r["message"]["role"].as_str().unwrap_or_default();
        if role != "user" && role != "assistant" {
            continue;
        }
        let rid = r["id"].as_str().unwrap_or_default();
        let at = r["timestamp"].as_str().map(str::to_string);
        for (i, b) in pi_blocks(&r["message"]).iter().enumerate() {
            let id = format!("{rid}:{i}");
            let entry = match b["type"].as_str().unwrap_or_default() {
                "text" => {
                    let kind = if role == "user" {
                        EntryKind::User
                    } else {
                        EntryKind::Assistant
                    };
                    text_entry(id, kind, b["text"].as_str().unwrap_or_default(), at.clone())
                }
                "thinking" if role == "assistant" => Some(thinking_entry(
                    id,
                    b["thinking"].as_str().unwrap_or_default(),
                    None,
                    at.clone(),
                )),
                "toolCall" if role == "assistant" => {
                    let tid = b["id"].as_str().unwrap_or_default();
                    let name = b["name"].as_str().unwrap_or_default();
                    Some(tool_entry(
                        id,
                        ToolLine {
                            id: tid.to_string(),
                            name: name.to_string(),
                            summary: tool_summary(name, &b["arguments"], cwd),
                            status: status_of(&results, tid),
                        },
                        at.clone(),
                    ))
                }
                "image" => Some(notice_entry(id, at.clone())),
                _ => None,
            };
            out.extend(entry);
        }
    }
    out
}

/// Full result text for one tool call, capped at 64 KiB.
pub fn tool_result(kind: TranscriptKind, text: &str, tool_id: &str) -> Option<ToolResult> {
    for r in parse_lines(text) {
        let (content, is_error) = match kind {
            TranscriptKind::ClaudeCode => {
                if r["type"] != "user" {
                    continue;
                }
                let Some(b) = claude_blocks(&r)
                    .into_iter()
                    .find(|b| b["type"] == "tool_result" && b["tool_use_id"] == tool_id)
                else {
                    continue;
                };
                (
                    b["content"].clone(),
                    b["is_error"].as_bool().unwrap_or(false),
                )
            }
            TranscriptKind::Pi => {
                if r["type"] != "message"
                    || r["message"]["role"] != "toolResult"
                    || r["message"]["toolCallId"] != tool_id
                {
                    continue;
                }
                (
                    r["message"]["content"].clone(),
                    r["message"]["isError"].as_bool().unwrap_or(false),
                )
            }
        };
        let full = result_text(&content);
        let truncated = full.len() > RESULT_CAP;
        return Some(ToolResult {
            text: cut(&full, RESULT_CAP).to_string(),
            is_error,
            truncated,
        });
    }
    None
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn clip(s: &str) -> String {
    if s.chars().count() > SUMMARY_CAP {
        let head: String = s.chars().take(SUMMARY_CAP).collect();
        format!("{head}…")
    } else {
        s.to_string()
    }
}

fn rel_path(p: &str, cwd: Option<&Path>) -> String {
    if let Some(cwd) = cwd {
        if let Ok(rest) = Path::new(p).strip_prefix(cwd) {
            return rest.to_string_lossy().into_owned();
        }
    }
    p.to_string()
}

fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', '#', ':'])
        .next()
        .unwrap_or(rest)
        .to_string()
}

/// One-line, human summary of a tool call's input.
pub fn tool_summary(name: &str, input: &Value, cwd: Option<&Path>) -> String {
    let s = |k: &str| input[k].as_str();
    let lower = name.to_ascii_lowercase();
    let known = match lower.as_str() {
        "edit" | "write" | "read" | "multiedit" | "notebookedit" => s("file_path")
            .or_else(|| s("path"))
            .map(|p| clip(&rel_path(p, cwd))),
        "bash" => s("command").map(|c| clip(c.lines().next().unwrap_or_default())),
        "grep" | "glob" => s("pattern").map(|p| clip(&format!("\"{p}\""))),
        "webfetch" => s("url").map(host_of),
        "websearch" => s("query").map(clip),
        "task" | "agent" => s("description").map(clip),
        _ => None,
    };
    if let Some(k) = known {
        return k;
    }
    if let Some(obj) = input.as_object() {
        for v in obj.values() {
            if let Some(t) = v.as_str() {
                return clip(t.lines().next().unwrap_or_default());
            }
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CLAUDE: &str = include_str!("../tests/fixtures/claude-transcript.jsonl");
    const PI: &str = include_str!("../tests/fixtures/pi-session-v3.jsonl");

    #[test]
    fn claude_text_thinking_and_tools() {
        let e = parse_entries(TranscriptKind::ClaudeCode, CLAUDE, None);
        assert_eq!(e.len(), 5);
        assert_eq!(e[0].kind, EntryKind::User);
        assert_eq!(e[0].text.as_deref(), Some("fix the test"));
        assert_eq!(e[0].id, "u1:0");
        assert_eq!(e[1].kind, EntryKind::Thinking);
        assert_eq!(e[1].text, None);
        assert_eq!(e[1].duration_ms, Some(12000));
        let t = e[2].tool.as_ref().unwrap();
        assert_eq!(
            (t.name.as_str(), t.summary.as_str()),
            ("Bash", "cargo test -p oo-remote")
        );
        assert_eq!(t.status, ToolStatus::Ok);
        let t = e[3].tool.as_ref().unwrap();
        assert_eq!(
            (t.name.as_str(), t.summary.as_str()),
            ("Edit", "tests/server.rs")
        );
        assert_eq!(t.status, ToolStatus::Error);
        assert_eq!(e[4].kind, EntryKind::Assistant);
        assert_eq!(e[4].text.as_deref(), Some("Fixed."));
    }

    #[test]
    fn thinking_text_is_capped() {
        let long = "x".repeat(5000);
        let line = json!({"type":"assistant","uuid":"a","parentUuid":null,
            "message":{"content":[{"type":"thinking","thinking":long}]}})
        .to_string();
        let e = parse_entries(TranscriptKind::ClaudeCode, &line, None);
        let t = e[0].text.as_ref().unwrap();
        assert_eq!(t.len(), 4096 + "…[truncated]".len());
        assert!(t.ends_with("…[truncated]"));
    }

    #[test]
    fn claude_skips_sidechain_meta_and_bookkeeping() {
        let lines = [
            r#"{"type":"attachment","uuid":"x1","parentUuid":null}"#,
            r#"{"type":"system","uuid":"x2","parentUuid":"x1"}"#,
            r#"{"type":"file-history-snapshot"}"#,
            r#"{"type":"user","uuid":"m1","parentUuid":"x2","isMeta":true,"message":{"content":"meta"}}"#,
            r#"{"type":"user","uuid":"s1","parentUuid":"m1","isSidechain":true,"message":{"content":"side"}}"#,
            r#"{"type":"user","uuid":"i1","parentUuid":"m1","message":{"content":[{"type":"image","source":{}}]}}"#,
        ]
        .join("\n");
        let e = parse_entries(TranscriptKind::ClaudeCode, &lines, None);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].kind, EntryKind::Notice);
        assert_eq!(e[0].text.as_deref(), Some("[image]"));
    }

    #[test]
    fn claude_tool_result_user_record_is_not_a_prompt() {
        let lines = r#"{"type":"user","uuid":"u","parentUuid":null,"message":{"content":[{"type":"tool_result","tool_use_id":"t","content":"x"}]}}"#;
        assert!(parse_entries(TranscriptKind::ClaudeCode, lines, None).is_empty());
    }

    #[test]
    fn claude_follows_active_branch() {
        let lines = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"content":"go"}}"#,
            r#"{"type":"assistant","uuid":"old","parentUuid":"u1","message":{"content":[{"type":"text","text":"old"}]}}"#,
            r#"{"type":"assistant","uuid":"new","parentUuid":"u1","message":{"content":[{"type":"text","text":"new"}]}}"#,
        ]
        .join("\n");
        let e = parse_entries(TranscriptKind::ClaudeCode, &lines, None);
        let texts: Vec<_> = e.iter().filter_map(|e| e.text.as_deref()).collect();
        assert_eq!(texts, ["go", "new"]);
    }

    #[test]
    fn pi_fixture_entries() {
        let e = parse_entries(TranscriptKind::Pi, PI, None);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].kind, EntryKind::User);
        assert_eq!(e[0].text.as_deref(), Some("run the probe"));
        assert_eq!(e[1].tool.as_ref().unwrap().status, ToolStatus::Ok);
        assert_eq!(
            e[1].tool.as_ref().unwrap().summary,
            "echo harness-lifecycle-probe"
        );
        assert_eq!(e[2].kind, EntryKind::Assistant);
        assert_eq!(e[2].text.as_deref(), Some("Probe complete."));
        assert!(e.iter().all(|e| e.at.is_some()));
    }

    #[test]
    fn tool_summary_per_tool() {
        let cwd = Some(Path::new("/repo"));
        for n in ["Edit", "Write", "Read"] {
            assert_eq!(
                tool_summary(n, &json!({"file_path": "/repo/src/a.rs"}), cwd),
                "src/a.rs"
            );
        }
        assert_eq!(
            tool_summary("Bash", &json!({"command": "cargo test\nmore"}), None),
            "cargo test"
        );
        let long = "a".repeat(100);
        assert_eq!(
            tool_summary("Bash", &json!({"command": long}), None),
            format!("{}…", "a".repeat(80))
        );
        assert_eq!(
            tool_summary("Grep", &json!({"pattern": "send_keys"}), None),
            "\"send_keys\""
        );
        assert_eq!(
            tool_summary("WebFetch", &json!({"url": "https://docs.rs/x/y"}), None),
            "docs.rs"
        );
        assert_eq!(tool_summary("Mystery", &json!({"q": "abc"}), None), "abc");
    }

    #[test]
    fn tool_result_is_capped() {
        let big = "y".repeat(200 * 1024);
        let line = json!({"type":"user","uuid":"u","parentUuid":null,"message":{"content":[
            {"type":"tool_result","tool_use_id":"t1","content":big}]}})
        .to_string();
        let r = tool_result(TranscriptKind::ClaudeCode, &line, "t1").unwrap();
        assert_eq!(r.text.len(), 64 * 1024);
        assert!(r.truncated);
        assert!(tool_result(TranscriptKind::ClaudeCode, &line, "nope").is_none());
        let p = tool_result(TranscriptKind::Pi, PI, "call_1").unwrap();
        assert_eq!(p.text, "harness-lifecycle-probe\n");
        assert!(!p.is_error && !p.truncated);
    }

    #[test]
    fn unknown_kinds_unsupported() {
        assert_eq!(TranscriptKind::from_agent_kind("codex"), None);
        assert_eq!(
            TranscriptKind::from_agent_kind("claude-code"),
            Some(TranscriptKind::ClaudeCode)
        );
        assert_eq!(
            TranscriptKind::from_agent_kind("pi"),
            Some(TranscriptKind::Pi)
        );
    }
}
