//! The process tree, for `#{pane_current_command}`: the program in the
//! foreground of a session, found by walking down from its root process
//! through shells (Git Bash's launcher, the msys fork it runs a command in)
//! to the first process that is not one.

use std::collections::HashMap;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    /// Executable name without `.exe`.
    pub name: String,
}

/// Every process on the machine. Empty on failure.
pub fn snapshot() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return out;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut ok = Process32FirstW(snap, &mut entry);
        while ok != 0 {
            let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
            let exe = String::from_utf16_lossy(&entry.szExeFile[..len]);
            out.push(Proc {
                pid: entry.th32ProcessID,
                ppid: entry.th32ParentProcessID,
                name: strip_exe(&exe),
            });
            ok = Process32NextW(snap, &mut entry);
        }
        CloseHandle(snap);
    }
    out
}

fn strip_exe(name: &str) -> String {
    match name.len().checked_sub(4) {
        Some(i) if name[i..].eq_ignore_ascii_case(".exe") => name[..i].to_string(),
        _ => name.to_string(),
    }
}

/// Shells, which the walk looks through.
pub fn is_shell(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "bash" | "sh" | "zsh" | "fish" | "dash" | "nu" | "cmd" | "powershell" | "pwsh" | "login"
    )
}

/// The foreground program under `root`: `root` itself when it is not a
/// shell; otherwise the newest non-shell child of the innermost shell, or
/// that shell when it is idle.
pub fn foreground(procs: &[Proc], root: u32) -> Option<String> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut current = *by_pid.get(&root)?;
    for _ in 0..32 {
        if !is_shell(&current.name) {
            return Some(current.name.clone());
        }
        let children: Vec<&Proc> = procs
            .iter()
            .filter(|p| p.ppid == current.pid && p.pid != current.pid && p.name != "conhost")
            .collect();
        if let Some(program) = children.iter().rev().find(|p| !is_shell(&p.name)) {
            return Some(program.name.clone());
        }
        match children.last() {
            Some(shell) => current = shell,
            None => return Some(current.name.clone()),
        }
    }
    Some(current.name.clone())
}

/// [`foreground`], falling back on the session's job `members` when the walk
/// ends at a shell.
///
/// Git Bash breaks the parent chain whenever a forked bash execs another
/// msys program (an npm `sh` shim such as `codex`): the fork exits and its
/// replacement's recorded parent is dead, so the walk from the root cannot
/// see the agent. Every process the session started is still in its job,
/// though, so the top-most non-shell member is the program. (A background
/// job left running at the prompt would count too; tmux would not count it.)
pub fn foreground_in(procs: &[Proc], root: u32, members: &[u32]) -> Option<String> {
    let walked = foreground(procs, root);
    if walked.as_deref().is_some_and(|n| !is_shell(n)) || members.is_empty() {
        return walked;
    }
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let programs: Vec<&Proc> = members
        .iter()
        .filter_map(|pid| by_pid.get(pid).copied())
        .filter(|p| !is_shell(&p.name) && p.name != "conhost" && p.name != "OpenConsole")
        .collect();
    programs
        .iter()
        .rev()
        .find(|p| !programs.iter().any(|q| q.pid == p.ppid))
        .map(|p| p.name.clone())
        .or(walked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, ppid: u32, name: &str) -> Proc {
        Proc { pid, ppid, name: name.into() }
    }

    #[test]
    fn walks_through_shells_to_the_program() {
        // Git Bash launcher → msys bash → its fork → claude (→ its children).
        let procs = vec![
            p(10, 1, "bash"),
            p(11, 10, "bash"),
            p(12, 11, "bash"),
            p(13, 12, "claude"),
            p(14, 13, "node"),
        ];
        assert_eq!(foreground(&procs, 10).as_deref(), Some("claude"));
        // Back at the prompt.
        let idle = vec![p(10, 1, "bash"), p(11, 10, "bash")];
        assert_eq!(foreground(&idle, 10).as_deref(), Some("bash"));
        assert_eq!(foreground(&idle, 99), None);
        assert_eq!(strip_exe("Claude.EXE"), "Claude");
    }

    #[test]
    fn a_broken_chain_is_found_through_the_job() {
        // bash (14712) forked 36912, which exec'd sh and exited: sh's
        // recorded parent is gone. node runs codex.exe under it.
        let procs = vec![
            p(10, 1, "bash"),
            p(14712, 10, "bash"),
            p(30328, 36912, "sh"),
            p(24912, 30328, "node"),
            p(29396, 24912, "codex"),
        ];
        let members = [10, 14712, 30328, 24912, 29396];
        assert_eq!(foreground(&procs, 10).as_deref(), Some("bash"));
        assert_eq!(foreground_in(&procs, 10, &members).as_deref(), Some("node"));
        // At the prompt: only shells in the job.
        assert_eq!(foreground_in(&procs, 10, &[10, 14712]).as_deref(), Some("bash"));
    }
}
