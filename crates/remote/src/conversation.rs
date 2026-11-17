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
    /// A shell tab's command log (see `crate::shell`).
    Shell,
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
/// the last such record is the tip. `parent_of` is followed through every
/// record.
fn active_branch<'a>(
    records: &'a [Value],
    id_key: &str,
    parent_of: impl Fn(&'a Value) -> Option<&'a str>,
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
    while let Some(parent) = parent_of(cur) {
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
    if kind == TranscriptKind::Shell {
        return crate::shell::parse_entries(text);
    }
    let records = parse_lines(text);
    match kind {
        TranscriptKind::ClaudeCode => claude_entries(&records, cwd),
        TranscriptKind::Pi => pi_entries(&records, cwd),
        TranscriptKind::Shell => unreachable!("handled above"),
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

fn notice_entry(id: String, text: &str, at: Option<String>) -> Entry {
    Entry {
        id,
        kind: EntryKind::Notice,
        text: Some(text.to_string()),
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

fn is_compact_boundary(r: &Value) -> bool {
    r["type"] == "system" && r["subtype"] == "compact_boundary"
}

/// Tags Claude Code wraps around slash commands and `!` shell commands in
/// user records, and (below) around their output.
const MARKUP_TAGS: [&str; 5] = [
    "command-name",
    "command-message",
    "command-args",
    "local-command-caveat",
    "bash-input",
];
const OUTPUT_TAGS: [&str; 4] = [
    "local-command-stdout",
    "local-command-stderr",
    "bash-stdout",
    "bash-stderr",
];

/// The text inside the first `<tag>…</tag>` in `s`.
fn tag_text<'a>(s: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = s.find(&open)? + open.len();
    let len = s[start..].find(&close)?;
    Some(s[start..start + len].trim())
}

/// User text that is Claude Code command markup, not a prompt: `None` for a
/// plain prompt, `Some(None)` to drop it, `Some(Some(notice))` to show it as
/// a notice. Only text that starts with a known tag counts, so a prompt
/// quoting a tag stays a prompt.
fn command_markup(text: &str) -> Option<Option<String>> {
    let t = text.trim_start();
    let known = MARKUP_TAGS
        .iter()
        .chain(OUTPUT_TAGS.iter())
        .any(|tag| t.starts_with(&format!("<{tag}>")));
    if !known {
        return None;
    }
    if let Some(name) = tag_text(t, "command-name") {
        let args = tag_text(t, "command-args").unwrap_or_default();
        let line = if args.is_empty() {
            name.to_string()
        } else {
            format!("{name} {args}")
        };
        return Some(Some(clip(line.lines().next().unwrap_or_default())));
    }
    if let Some(cmd) = tag_text(t, "bash-input") {
        return Some(Some(clip(&format!(
            "! {}",
            cmd.lines().next().unwrap_or_default()
        ))));
    }
    let out = OUTPUT_TAGS
        .iter()
        .filter_map(|tag| tag_text(t, tag))
        .find(|o| !o.is_empty())
        .and_then(|o| o.lines().map(str::trim).find(|l| !l.is_empty()));
    Some(out.map(clip))
}

/// A message the user typed while Claude was working, delivered mid-turn
/// (`{"type":"attachment","attachment":{"type":"queued_command",
/// "prompt":…,"origin":{"kind":"human"}}}`), as the user's entry. Queued
/// notifications from tools (other origins) are not the user's.
fn queued_message(r: &Value) -> Option<Entry> {
    let a = &r["attachment"];
    if r["type"] != "attachment" || a["type"] != "queued_command" || a["origin"]["kind"] != "human"
    {
        return None;
    }
    let text = match &a["prompt"] {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    let id = format!("{}:queued", r["uuid"].as_str().unwrap_or_default());
    let at = a["timestamp"]
        .as_str()
        .or_else(|| r["timestamp"].as_str())
        .map(str::to_string);
    text_entry(id, EntryKind::User, &text, at)
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
    let branch = active_branch(
        records,
        "uuid",
        // A compaction boundary cuts `parentUuid`; `logicalParentUuid` keeps
        // the history before it reachable.
        |r| {
            r["parentUuid"].as_str().or_else(|| {
                is_compact_boundary(r)
                    .then(|| r["logicalParentUuid"].as_str())
                    .flatten()
            })
        },
        |r| (r["type"] == "user" || r["type"] == "assistant") && r["isSidechain"] != true,
    );
    let mut out = Vec::new();
    for r in branch {
        let role = r["type"].as_str().unwrap_or_default();
        if is_compact_boundary(r) {
            let id = format!("{}:compact", r["uuid"].as_str().unwrap_or_default());
            let at = r["timestamp"].as_str().map(str::to_string);
            out.push(notice_entry(id, "Conversation compacted", at));
            continue;
        }
        // A message typed while Claude was working is often handed to it
        // mid-turn as a `queued_command` attachment, never as a user record.
        if let Some(entry) = queued_message(r) {
            out.push(entry);
            continue;
        }
        // The post-compaction summary is written as a user record; it is for
        // the model, not the reader (and names the transcript's path).
        if (role != "user" && role != "assistant")
            || r["isSidechain"] == true
            || r["isMeta"] == true
            || r["isCompactSummary"] == true
            || r["isVisibleInTranscriptOnly"] == true
        {
            continue;
        }
        let uuid = r["uuid"].as_str().unwrap_or_default();
        let at = r["timestamp"].as_str().map(str::to_string);
        for (i, b) in claude_blocks(r).iter().enumerate() {
            let id = format!("{uuid}:{i}");
            let entry = match b["type"].as_str().unwrap_or_default() {
                "text" if role == "user" => {
                    let text = b["text"].as_str().unwrap_or_default();
                    match command_markup(text) {
                        Some(Some(notice)) => Some(notice_entry(id, &notice, at.clone())),
                        Some(None) => None,
                        None => text_entry(id, EntryKind::User, text, at.clone()),
                    }
                }
                "text" => text_entry(
                    id,
                    EntryKind::Assistant,
                    b["text"].as_str().unwrap_or_default(),
                    at.clone(),
                ),
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
                "image" => Some(notice_entry(id, "[image]", at.clone())),
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
    let branch = active_branch(
        records,
        "id",
        |r| r["parentId"].as_str(),
        |r| r["type"] == "message",
    );
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
                "image" => Some(notice_entry(id, "[image]", at.clone())),
                _ => None,
            };
            out.extend(entry);
        }
    }
    out
}

/// Full result text for one tool call, capped at 64 KiB.
pub fn tool_result(kind: TranscriptKind, text: &str, tool_id: &str) -> Option<ToolResult> {
    if kind == TranscriptKind::Shell {
        return crate::shell::tool_result(text, tool_id);
    }
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
            TranscriptKind::Shell => unreachable!("handled above"),
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
    let summary = known.or_else(|| {
        input.as_object().and_then(|obj| {
            obj.values()
                .find_map(Value::as_str)
                .map(|t| clip(t.lines().next().unwrap_or_default()))
        })
    });
    match summary {
        Some(s) if !s.trim().is_empty() => s,
        // No string input (TodoWrite's list) or a blank one: the tool's name.
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_queued_while_claude_works_show_as_the_users() {
        let log = r#"{"type":"user","uuid":"u1","parentUuid":null,"isSidechain":false,"timestamp":"2026-11-17T01:00:00Z","message":{"role":"user","content":"start"}}
{"type":"assistant","uuid":"a1","parentUuid":"u1","isSidechain":false,"timestamp":"2026-11-17T01:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"Working."}]}}
{"type":"attachment","uuid":"q1","parentUuid":"a1","isSidechain":false,"timestamp":"2026-11-17T01:00:05Z","attachment":{"type":"queued_command","prompt":"then start the app","origin":{"kind":"human"},"timestamp":"2026-11-17T01:00:05Z"}}
{"type":"attachment","uuid":"q2","parentUuid":"q1","isSidechain":false,"timestamp":"2026-11-17T01:00:06Z","attachment":{"type":"queued_command","prompt":"<task-notification>done</task-notification>","origin":{"kind":"task"}}}
{"type":"assistant","uuid":"a2","parentUuid":"q2","isSidechain":false,"timestamp":"2026-11-17T01:00:07Z","message":{"role":"assistant","content":[{"type":"text","text":"Starting it."}]}}
"#;
        let e = parse_entries(TranscriptKind::ClaudeCode, log, None);
        let users: Vec<_> = e
            .iter()
            .filter(|e| e.kind == EntryKind::User)
            .map(|e| (e.text.as_deref().unwrap(), e.at.as_deref()))
            .collect();
        assert_eq!(
            users,
            vec![
                ("start", Some("2026-11-17T01:00:00Z")),
                ("then start the app", Some("2026-11-17T01:00:05Z")),
            ]
        );
        // In order, between Claude's replies.
        let texts: Vec<_> = e.iter().filter_map(|e| e.text.as_deref()).collect();
        assert_eq!(
            texts,
            vec!["start", "Working.", "then start the app", "Starting it."]
        );
    }
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

    const COMPACT: &str = include_str!("../tests/fixtures/claude-compact.jsonl");

    fn shown(e: &[Entry]) -> Vec<(EntryKind, String)> {
        e.iter()
            .map(|e| {
                let t = e
                    .text
                    .clone()
                    .or_else(|| e.tool.as_ref().map(|t| t.summary.clone()))
                    .unwrap_or_default();
                (e.kind, t)
            })
            .collect()
    }

    #[test]
    fn compaction_shows_a_notice_keeps_history_and_hides_the_summary() {
        let e = parse_entries(TranscriptKind::ClaudeCode, COMPACT, None);
        assert!(
            e.iter()
                .all(|e| !e.text.as_deref().unwrap_or_default().contains(".jsonl")),
            "{e:?}"
        );
        let got = shown(&e);
        let n = |s: &str| (EntryKind::Notice, s.to_string());
        assert_eq!(
            got,
            [
                (EntryKind::User, "rename the widget".to_string()),
                (EntryKind::Assistant, "Renamed it.".to_string()),
                n("/compact"),
                n("Conversation compacted"),
                n("Compacted. ctrl+o to see full summary"),
                n("! git status"),
                n("On branch main"),
                (EntryKind::User, "now add a test".to_string()),
                (EntryKind::Tool, "TodoWrite".to_string()),
            ]
        );
        let ids: HashSet<_> = e.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids.len(), e.len(), "ids are unique");
    }

    #[test]
    fn compaction_without_the_earlier_history_still_shows_the_notice() {
        // The 8 MiB window may start after the boundary's logical parent.
        let tail: String = COMPACT.lines().skip(3).collect::<Vec<_>>().join("\n");
        let got = shown(&parse_entries(TranscriptKind::ClaudeCode, &tail, None));
        assert_eq!(
            got[0],
            (EntryKind::Notice, "Conversation compacted".to_string())
        );
        assert_eq!(got.len(), 6);
    }

    #[test]
    fn slash_command_markup_never_shows_raw_tags() {
        let rec = |c: &str| {
            json!({"type":"user","uuid":"u","parentUuid":null,"message":{"content":c}}).to_string()
        };
        let cases = [
            (
                "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>src/a.rs</command-args>",
                Some("/review src/a.rs"),
            ),
            ("<local-command-stdout></local-command-stdout>", None),
            ("<local-command-caveat>Caveat: ignore</local-command-caveat>", None),
            ("<bash-stderr>boom\nmore</bash-stderr>", Some("boom")),
        ];
        for (content, want) in cases {
            let e = parse_entries(TranscriptKind::ClaudeCode, &rec(content), None);
            assert_eq!(
                e.iter().filter_map(|e| e.text.as_deref()).next(),
                want,
                "{content}"
            );
            assert!(e.iter().all(|e| e.kind == EntryKind::Notice));
        }
        // A prompt that merely quotes a tag stays a prompt.
        let quoted = "why does <bash-input> show up?";
        let e = parse_entries(TranscriptKind::ClaudeCode, &rec(quoted), None);
        assert_eq!(e[0].kind, EntryKind::User);
        assert_eq!(e[0].text.as_deref(), Some(quoted));
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
        // No string input (or a blank one): the tool's name, never a blank line.
        assert_eq!(
            tool_summary("TodoWrite", &json!({"todos": [{"content": "x"}]}), None),
            "TodoWrite"
        );
        assert_eq!(
            tool_summary("Bash", &json!({"command": "  "}), None),
            "Bash"
        );
        assert_eq!(tool_summary("Mystery", &json!({}), None), "Mystery");
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
