//! What an agent is doing right now.
//!
//! Only Claude Code for now, from two sources:
//! - its PreToolUse hook reports the tool it is *about* to run (status.json's
//!   `tool`), which is the live answer while that call is in flight — Claude
//!   Code writes a tool call to its transcript only once it has finished;
//! - its transcript (`transcript_path`, JSONL, one content block per line)
//!   says when that call finished, and what came after: thinking, or
//!   writing a reply.
//!
//! Anything that doesn't parse returns `None` and the caption falls back to a
//! plain "Working", so a transcript format change degrades, never breaks.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// How much of the file's end to read. One line holds one content block, and
/// a big tool result can be tens of KB, so leave room for a few of those.
const TAIL_BYTES: u64 = 256 * 1024;

/// The current activity: the announced tool while its result hasn't landed in
/// the transcript, otherwise whatever the transcript's newest step says.
pub fn current(
    tool: Option<&agent_runtime::ToolCall>,
    transcript: Option<&Path>,
) -> Option<String> {
    let tail = transcript.and_then(read_tail);
    if let Some(tool) = tool {
        let finished = match (&tool.id, &tail) {
            (Some(id), Some(tail)) => tail.contains(&format!("\"tool_use_id\":\"{id}\"")),
            _ => false,
        };
        if !finished {
            return Some(describe_tool(&tool.name, &tool.input));
        }
    }
    tail.as_deref().and_then(activity_from_tail)
}

/// The end of a transcript, starting at a line boundary.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // A mid-file start lands inside a line; drop that partial first line.
    Some(if start > 0 {
        text.split_once('\n')
            .map_or("", |(_, rest)| rest)
            .to_string()
    } else {
        text.into_owned()
    })
}

/// Pure core of [`read_activity`]: scan JSONL lines from the end for the
/// newest main-thread step.
pub fn activity_from_tail(tail: &str) -> Option<String> {
    for line in tail.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let kind = entry.get("type").and_then(Value::as_str);
        let content = entry.pointer("/message/content");
        match kind {
            Some("assistant") => {
                let block = content.and_then(Value::as_array).and_then(|b| b.last())?;
                return Some(match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => describe_tool(
                        block.get("name").and_then(Value::as_str).unwrap_or(""),
                        block.get("input").unwrap_or(&Value::Null),
                    ),
                    Some("text") => "Writing a reply".to_string(),
                    _ => "Thinking".to_string(),
                });
            }
            // A tool result or a fresh prompt: the model is deciding what
            // to do next.
            Some("user") => return Some("Thinking".to_string()),
            _ => continue,
        }
    }
    None
}

/// The last path component, for "Editing main.rs" rather than a full path.
fn file_name(input: &Value, key: &str) -> Option<String> {
    let p = input.get(key)?.as_str()?;
    Some(Path::new(p).file_name()?.to_string_lossy().into_owned())
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{}…", cut.trim_end())
    }
}

/// The program a shell command runs, skipping `cd somewhere &&` and env
/// assignments: "cargo test", not "cd /long/path && FOO=1 cargo test -q".
fn command_head(cmd: &str) -> String {
    let segment = cmd
        .split("&&")
        .map(str::trim)
        .find(|s| !s.is_empty() && !s.starts_with("cd "))
        .unwrap_or(cmd);
    let words: Vec<&str> = segment
        .split_whitespace()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'))
        .filter(|w| !w.starts_with('-'))
        .take(2)
        .collect();
    let head = words.join(" ");
    // Bare paths read badly; keep only the binary's name.
    let head = head
        .split(' ')
        .map(|w| {
            if w.contains('/') {
                w.rsplit('/').next().unwrap_or(w)
            } else {
                w
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    clip(&head, 24)
}

pub fn describe_tool(name: &str, input: &Value) -> String {
    let file = |key| file_name(input, key);
    match name {
        "Read" => file("file_path").map_or("Reading".into(), |f| format!("Reading {f}")),
        "Edit" | "MultiEdit" => {
            file("file_path").map_or("Editing".into(), |f| format!("Editing {f}"))
        }
        "NotebookEdit" => {
            file("notebook_path").map_or("Editing a notebook".into(), |f| format!("Editing {f}"))
        }
        "Write" => file("file_path").map_or("Writing".into(), |f| format!("Writing {f}")),
        "Bash" => match input.get("command").and_then(Value::as_str) {
            Some(cmd) if !command_head(cmd).is_empty() => format!("Running {}", command_head(cmd)),
            _ => "Running a command".into(),
        },
        "Grep" => match input.get("pattern").and_then(Value::as_str) {
            Some(p) => format!("Searching for “{}”", clip(p, 18)),
            None => "Searching the code".into(),
        },
        "Glob" | "LS" => "Looking through files".into(),
        "WebSearch" => "Searching the web".into(),
        "WebFetch" => "Reading a web page".into(),
        "Task" | "Agent" => "Briefing a helper".into(),
        "TodoWrite" | "TaskCreate" | "TaskUpdate" => "Updating its plan".into(),
        "AskUserQuestion" => "Asking you something".into(),
        "Skill" => "Reading up on a skill".into(),
        "ExitPlanMode" => "Proposing a plan".into(),
        n if n.starts_with("mcp__") => {
            let tool = n.rsplit("__").next().unwrap_or(n).replace('_', " ");
            format!("Using {}", clip(&tool, 22))
        }
        "" => "Working".into(),
        n => format!("Using {}", clip(n, 22)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(v: serde_json::Value) -> String {
        format!("{}\n", v)
    }

    fn tool(name: &str, input: serde_json::Value) -> String {
        line(serde_json::json!({
            "type": "assistant",
            "isSidechain": false,
            "message": { "content": [{ "type": "tool_use", "name": name, "input": input }] }
        }))
    }

    #[test]
    fn a_tool_in_flight_is_described() {
        let tail = tool(
            "Edit",
            serde_json::json!({ "file_path": "/a/b/workspace.rs" }),
        );
        assert_eq!(
            activity_from_tail(&tail).as_deref(),
            Some("Editing workspace.rs")
        );
    }

    #[test]
    fn after_a_tool_result_it_is_thinking() {
        let tail = tool("Read", serde_json::json!({ "file_path": "/x.rs" }))
            + &line(serde_json::json!({
                "type": "user",
                "message": { "content": [{ "type": "tool_result", "content": "..." }] }
            }));
        assert_eq!(activity_from_tail(&tail).as_deref(), Some("Thinking"));
    }

    #[test]
    fn bookkeeping_lines_and_sidechains_are_skipped() {
        let tail = tool("Grep", serde_json::json!({ "pattern": "fn main" }))
            + &line(serde_json::json!({ "type": "attachment" }))
            + &line(serde_json::json!({
                "type": "assistant",
                "isSidechain": true,
                "message": { "content": [{ "type": "text", "text": "sub" }] }
            }))
            + "not json at all\n";
        assert_eq!(
            activity_from_tail(&tail).as_deref(),
            Some("Searching for “fn main”")
        );
    }

    #[test]
    fn shell_commands_show_the_program_not_the_noise() {
        assert_eq!(
            command_head("cd /very/long/path && RUST_LOG=1 cargo test -q"),
            "cargo test"
        );
        assert_eq!(command_head("/usr/bin/git status --short"), "git status");
        let tail = tool(
            "Bash",
            serde_json::json!({ "command": "cd /x && make run" }),
        );
        assert_eq!(
            activity_from_tail(&tail).as_deref(),
            Some("Running make run")
        );
    }

    #[test]
    fn thinking_and_text_blocks() {
        let think = line(serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "thinking", "thinking": "" }] }
        }));
        assert_eq!(activity_from_tail(&think).as_deref(), Some("Thinking"));
        let text = line(serde_json::json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "done" }] }
        }));
        assert_eq!(
            activity_from_tail(&text).as_deref(),
            Some("Writing a reply")
        );
    }

    #[test]
    fn mcp_tools_name_the_tool() {
        let tail = tool("mcp__github__create_pull_request", serde_json::json!({}));
        assert_eq!(
            activity_from_tail(&tail).as_deref(),
            Some("Using create pull request")
        );
    }

    #[test]
    fn the_announced_tool_wins_until_its_result_lands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let call = agent_runtime::ToolCall {
            name: "Bash".into(),
            id: Some("toolu_9".into()),
            input: serde_json::json!({ "command": "cargo build" }),
        };
        std::fs::write(
            &path,
            tool("Read", serde_json::json!({ "file_path": "/old.rs" })),
        )
        .unwrap();
        assert_eq!(
            current(Some(&call), Some(&path)).as_deref(),
            Some("Running cargo build")
        );

        let done = line(serde_json::json!({
            "type": "user",
            "message": { "content": [{ "type": "tool_result", "tool_use_id": "toolu_9" }] }
        }));
        std::fs::write(&path, done).unwrap();
        assert_eq!(
            current(Some(&call), Some(&path)).as_deref(),
            Some("Thinking")
        );
        assert_eq!(current(None, None), None);
    }

    #[test]
    fn nothing_recognizable_is_none() {
        assert_eq!(activity_from_tail("{\"type\":\"summary\"}\n"), None);
        assert_eq!(activity_from_tail(""), None);
    }
}
