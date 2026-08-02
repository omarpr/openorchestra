//! Inject harness task context into an agent launch command without writing
//! into the project worktree.
//!
//! Multi-member sibling paths live under `~/.openorchestra/tasks/<slug>/`.
//! Each adapter that supports a system-prompt / developer-instructions flag
//! gets that file wired in; Cursor / OpenCode / custom agents get a positional
//! initial prompt that inlines the same text.

use std::fs;
use std::path::Path;

use crate::{AgentCommand, AgentKind};

/// Env var pointing at the harness-owned context file (absolute path).
pub const HARNESS_TASK_CONTEXT: &str = "HARNESS_TASK_CONTEXT";

/// Build the user-facing prompt that forces the agent to load task context.
pub fn task_context_prompt(contents: &str) -> String {
    format!("OpenOrchestra task context (read and follow):\n\n{contents}")
}

/// Append task-context injection to a structured [`AgentCommand`].
///
/// Always sets [`HARNESS_TASK_CONTEXT`] in `cmd.env`. Adapter-specific args
/// follow what each CLI actually supports for extra instructions.
pub fn apply_task_context(cmd: &mut AgentCommand, kind: &AgentKind, context_path: &Path) {
    let path_str = context_path.to_string_lossy().into_owned();
    cmd.env
        .retain(|(k, _)| k != HARNESS_TASK_CONTEXT);
    cmd.env
        .push((HARNESS_TASK_CONTEXT.to_string(), path_str.clone()));

    let contents = match fs::read_to_string(context_path) {
        Ok(c) if !c.trim().is_empty() => c,
        _ => return,
    };
    let prompt = task_context_prompt(&contents);

    match kind {
        AgentKind::ClaudeCode => {
            cmd.args
                .push("--append-system-prompt-file".to_string());
            cmd.args.push(path_str);
        }
        AgentKind::Pi => {
            // Pi accepts a file path or literal text; prefer the path so argv
            // stays short and the file remains the source of truth.
            cmd.args.push("--append-system-prompt".to_string());
            cmd.args.push(path_str);
        }
        AgentKind::Codex => {
            // Point Codex at the file via developer_instructions rather than
            // embedding a multi-line string in `-c` (escaping is fragile).
            cmd.args.push("-c".to_string());
            cmd.args.push(format!(
                "developer_instructions={}",
                toml_basic_string(&format!(
                    "Read and follow the openOrchestra task context file at {path_str}"
                ))
            ));
        }
        AgentKind::Kimi => {
            cmd.args.push("--system-prompt".to_string());
            cmd.args.push(prompt);
        }
        // OpenCode has no dedicated append flag; Other covers cursor-agent etc.
        // when launched via the structured adapter path (no prefs override).
        AgentKind::OpenCode | AgentKind::Other(_) => {
            cmd.args.push(prompt);
        }
    }
}

/// Augment a prefs command-override shell line so Cursor / custom agents still
/// receive the context as a positional prompt (their CLIs lack append flags).
///
/// `base_cmd` is the user's typed override (e.g. `cursor-agent`). The result
/// relies on the shell expanding `$(cat …)` after `HARNESS_TASK_CONTEXT` is
/// already in the pane environment — but we embed the absolute path directly
/// so the expansion does not depend on env timing inside `$SHELL -lc`.
pub fn shell_command_with_task_context(base_cmd: &str, context_path: &Path) -> String {
    let path = context_path.to_string_lossy();
    let quoted_path = shell_single_quote(&path);
    format!(
        r#"{base} "OpenOrchestra task context (read and follow): $(cat {quoted_path})""#,
        base = base_cmd.trim_end(),
        quoted_path = quoted_path,
    )
}

fn shell_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// TOML basic string literal suitable for `codex -c key=<value>`.
fn toml_basic_string(s: &str) -> String {
    let mut out = String::from('"');
    for ch in s.chars() {
        match ch {
            '"' | '\\' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn empty_cmd() -> AgentCommand {
        AgentCommand {
            program: PathBuf::from("/usr/bin/claude"),
            args: vec!["--permission-mode".into(), "auto".into()],
            env: Vec::new(),
            label: "claude".into(),
        }
    }

    #[test]
    fn claude_gets_append_system_prompt_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "# Task: t\nAlso:\n- api: `/wt/api`\n").unwrap();
        let mut cmd = empty_cmd();
        apply_task_context(&mut cmd, &AgentKind::ClaudeCode, &path);
        assert!(cmd.args.windows(2).any(|w| {
            w[0] == "--append-system-prompt-file" && w[1] == path.to_string_lossy()
        }));
        assert!(cmd
            .env
            .iter()
            .any(|(k, v)| k == HARNESS_TASK_CONTEXT && v == &path.to_string_lossy()));
    }

    #[test]
    fn pi_gets_append_system_prompt_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "siblings\n").unwrap();
        let mut cmd = empty_cmd();
        apply_task_context(&mut cmd, &AgentKind::Pi, &path);
        assert!(cmd.args.windows(2).any(|w| {
            w[0] == "--append-system-prompt" && w[1] == path.to_string_lossy()
        }));
    }

    #[test]
    fn cursor_like_other_gets_positional_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "Home (cwd): jam\nAlso:\n- api: `/x`\n").unwrap();
        let mut cmd = AgentCommand {
            program: PathBuf::from("cursor-agent"),
            args: Vec::new(),
            env: Vec::new(),
            label: "cursor-agent".into(),
        };
        apply_task_context(
            &mut cmd,
            &AgentKind::Other("cursor-agent".into()),
            &path,
        );
        assert_eq!(cmd.args.len(), 1);
        assert!(cmd.args[0].contains("OpenOrchestra task context"));
        assert!(cmd.args[0].contains("Home (cwd): jam"));
    }

    #[test]
    fn shell_override_appends_cat_prompt() {
        let s = shell_command_with_task_context(
            "cursor-agent",
            Path::new("/tmp/tasks/auth/agent-context.md"),
        );
        assert!(s.starts_with("cursor-agent \"OpenOrchestra task context"));
        assert!(s.contains("$(cat '/tmp/tasks/auth/agent-context.md')"));
    }

    #[test]
    fn codex_gets_developer_instructions_pointing_at_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "Also:\n- api: `/wt`\n").unwrap();
        let mut cmd = empty_cmd();
        apply_task_context(&mut cmd, &AgentKind::Codex, &path);
        assert!(cmd.args.windows(2).any(|w| {
            w[0] == "-c"
                && w[1].starts_with("developer_instructions=")
                && w[1].contains(&*path.to_string_lossy())
        }));
    }

    #[test]
    fn kimi_gets_system_prompt_with_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "Home (cwd): jam\n").unwrap();
        let mut cmd = empty_cmd();
        apply_task_context(&mut cmd, &AgentKind::Kimi, &path);
        assert!(cmd.args.windows(2).any(|w| {
            w[0] == "--system-prompt" && w[1].contains("Home (cwd): jam")
        }));
    }

    #[test]
    fn empty_context_file_still_sets_env_but_skips_args() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent-context.md");
        fs::write(&path, "   \n").unwrap();
        let mut cmd = empty_cmd();
        let before = cmd.args.clone();
        apply_task_context(&mut cmd, &AgentKind::ClaudeCode, &path);
        assert_eq!(cmd.args, before);
        assert!(cmd.env.iter().any(|(k, _)| k == HARNESS_TASK_CONTEXT));
    }
}
