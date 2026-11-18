//! Listening TCP ports for the tools panel's Ports tab.
//!
//! One `lsof` call lists every listener on the Mac; `ps` supplies each
//! process's executable path and parent; the tmux pane table says which
//! process trees belong to which worktree. [`classify`] joins them into
//! "this task" and "other on this Mac". Everything but [`sample`] is pure so
//! the attribution is testable without a live machine.

use std::collections::{BTreeMap, HashSet};

/// One listening socket as `lsof` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Listener {
    pub pid: u32,
    /// `*`, `127.0.0.1`, `::1`, … (IPv6 brackets stripped).
    pub addr: String,
    pub port: u16,
}

/// A process with its full executable path, from `ps -axo pid=,ppid=,comm=`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcPath {
    pub pid: u32,
    pub parent: Option<u32>,
    pub path: String,
}

/// One tick's raw inputs, collected off the UI thread.
#[derive(Clone, Debug, Default)]
pub struct PortsSample {
    pub app_pid: u32,
    pub listeners: Vec<Listener>,
    pub procs: Vec<ProcPath>,
    pub session_panes: Vec<(String, u32)>,
}

/// A worktree (or coordinator) whose tmux sessions count as "this task".
#[derive(Clone, Debug)]
pub struct Scope {
    pub label: String,
    pub session_prefix: String,
}

/// One port shown in the tab: a process listening on a port, on one or more
/// addresses.
#[derive(Clone, Debug, PartialEq)]
pub struct PortRow {
    pub port: u16,
    pub addrs: Vec<String>,
    pub pid: u32,
    pub name: String,
    /// The task member it runs in, only when the task has several.
    pub member: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PortsView {
    pub task: Vec<PortRow>,
    pub other: Vec<PortRow>,
}

/// Parse `lsof -nP -iTCP -sTCP:LISTEN -F pcn` output.
pub fn parse_lsof(out: &str) -> Vec<Listener> {
    let mut pid: Option<u32> = None;
    let mut found = Vec::new();
    for line in out.lines() {
        let Some(tag) = line.chars().next() else { continue };
        let rest = &line[tag.len_utf8()..];
        match tag {
            'p' => pid = rest.parse().ok(),
            'n' => {
                let (Some(pid), Some((addr, port))) = (pid, rest.rsplit_once(':')) else {
                    continue;
                };
                let Ok(port) = port.parse::<u16>() else { continue };
                let addr = addr.trim_start_matches('[').trim_end_matches(']');
                found.push(Listener { pid, addr: addr.to_string(), port });
            }
            _ => {}
        }
    }
    found
}

/// Parse `ps -axo pid=,ppid=,comm=` output, keeping the full path.
pub fn parse_ps_paths(out: &str) -> Vec<ProcPath> {
    out.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (pid, rest) = line.split_once(char::is_whitespace)?;
            let (ppid, path) = rest.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let ppid: u32 = ppid.parse().ok()?;
            let path = path.trim();
            if path.is_empty() {
                return None;
            }
            Some(ProcPath { pid, parent: (ppid != 0).then_some(ppid), path: path.to_string() })
        })
        .collect()
}

/// Executables under these belong to macOS itself (AirPlay receiver,
/// rapportd, …), never to something the user would share.
const SYSTEM_PREFIXES: &[&str] = &["/System/", "/usr/libexec/"];

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Split listeners into the selected task's and everything else's.
pub fn classify(s: &PortsSample, scopes: &[Scope]) -> PortsView {
    let path_of = |pid: u32| s.procs.iter().find(|p| p.pid == pid).map(|p| p.path.as_str());
    let edges: Vec<(u32, Option<u32>)> = s.procs.iter().map(|p| (p.pid, p.parent)).collect();

    // pid -> scope label, for every process under one of the scopes' panes.
    let mut owner: std::collections::HashMap<u32, &str> = std::collections::HashMap::new();
    for sc in scopes {
        let roots: Vec<u32> = s
            .session_panes
            .iter()
            .filter(|(name, _)| name.starts_with(&sc.session_prefix))
            .map(|(_, pid)| *pid)
            .collect();
        for pid in crate::resource_monitor::descendants(edges.iter().copied(), &roots) {
            owner.entry(pid).or_insert(&sc.label);
        }
    }

    // (port, pid) -> addresses, ordered by port then pid.
    let mut grouped: BTreeMap<(u16, u32), Vec<String>> = BTreeMap::new();
    for l in &s.listeners {
        let addrs = grouped.entry((l.port, l.pid)).or_default();
        if !addrs.contains(&l.addr) {
            addrs.push(l.addr.clone());
        }
    }

    let agents: HashSet<&str> = crate::preflight::AGENTS.iter().copied().collect();
    let mut view = PortsView::default();
    for ((port, pid), mut addrs) in grouped {
        addrs.sort();
        let path = path_of(pid).unwrap_or("");
        let name = if path.is_empty() { "?".to_string() } else { basename(path).to_string() };
        match owner.get(&pid) {
            Some(label) => {
                if agents.contains(name.to_ascii_lowercase().as_str()) {
                    continue;
                }
                let member = (scopes.len() > 1).then(|| label.to_string());
                view.task.push(PortRow { port, addrs, pid, name, member });
            }
            None => {
                if pid == s.app_pid || SYSTEM_PREFIXES.iter().any(|p| path.starts_with(p)) {
                    continue;
                }
                view.other.push(PortRow { port, addrs, pid, name, member: None });
            }
        }
    }
    view
}

/// The browser URL for a local port.
pub fn url(port: u16) -> String {
    format!("http://localhost:{port}")
}

/// Run `lsof`, `ps` and (when available) `tmux list-panes`. Blocking: call
/// it off the UI thread. A tool that fails or is missing contributes nothing.
pub fn sample(has_tmux: bool) -> PortsSample {
    let stdout = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    };
    // lsof exits 1 when nothing matches; its stdout is still the answer.
    let listeners = parse_lsof(&stdout("lsof", &["-nP", "-w", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"]));
    let procs = parse_ps_paths(&stdout("ps", &["-axo", "pid=,ppid=,comm="]));
    let session_panes = if has_tmux { crate::tmux::list_session_panes() } else { Vec::new() };
    PortsSample { app_pid: std::process::id(), listeners, procs, session_panes }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(pid: u32, addr: &str, port: u16) -> Listener {
        Listener { pid, addr: addr.into(), port }
    }

    fn p(pid: u32, parent: u32, path: &str) -> ProcPath {
        ProcPath { pid, parent: (parent != 0).then_some(parent), path: path.into() }
    }

    fn scope(prefix: &str, label: &str) -> Scope {
        Scope { label: label.into(), session_prefix: prefix.into() }
    }

    #[test]
    fn parse_lsof_reads_pid_addr_port() {
        let out = "p1403\ncControlCenter\nf6\nn*:7000\nf7\nn*:7000\np3941\nc2.1.327\nf20\nn127.0.0.1:54252\np5\ncnode\nf9\nn[::1]:5173\n";
        assert_eq!(
            parse_lsof(out),
            vec![
                l(1403, "*", 7000),
                l(1403, "*", 7000),
                l(3941, "127.0.0.1", 54252),
                l(5, "::1", 5173),
            ]
        );
    }

    #[test]
    fn parse_lsof_ignores_garbage() {
        assert!(parse_lsof("n*:x\nhello\np\n").is_empty());
        assert!(parse_lsof("n*:80\n").is_empty(), "a name before any pid has no owner");
    }

    #[test]
    fn parse_ps_paths_keeps_spaces() {
        let out = "  41084     1 /Applications/OrbStack.app/Contents/Frameworks/OrbStack Helper.app/Contents/MacOS/OrbStack Helper\n    1     0 /sbin/launchd\n";
        let procs = parse_ps_paths(out);
        assert_eq!(procs.len(), 2);
        assert_eq!(procs[0].pid, 41084);
        assert_eq!(procs[0].parent, Some(1));
        assert!(procs[0].path.ends_with("MacOS/OrbStack Helper"), "{}", procs[0].path);
        assert_eq!(procs[1].parent, None);
    }

    #[test]
    fn classify_dedupes_v4_v6() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(10, "::1", 5173), l(10, "*", 5173), l(10, "*", 5173)],
            procs: vec![p(10, 0, "/opt/homebrew/bin/node")],
            session_panes: vec![],
        };
        let v = classify(&s, &[]);
        assert_eq!(
            v.other,
            vec![PortRow {
                port: 5173,
                addrs: vec!["*".into(), "::1".into()],
                pid: 10,
                name: "node".into(),
                member: None,
            }]
        );
    }

    #[test]
    fn classify_attributes_deep_descendants_to_task() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(103, "*", 3000)],
            procs: vec![
                p(100, 50, "/bin/zsh"),
                p(101, 100, "/bin/zsh"),
                p(102, 101, "/opt/homebrew/bin/npm"),
                p(103, 102, "/opt/homebrew/bin/node"),
            ],
            session_panes: vec![("oo-abc-1".into(), 100)],
        };
        let v = classify(&s, &[scope("oo-abc", "web")]);
        assert_eq!(v.task.len(), 1);
        assert_eq!(v.task[0].port, 3000);
        assert_eq!(v.task[0].member, None, "one scope: no member label");
        assert!(v.other.is_empty());
    }

    #[test]
    fn classify_hides_agent_own_port() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(101, "127.0.0.1", 54000)],
            procs: vec![p(100, 50, "/bin/zsh"), p(101, 100, "/x/claude.app/Contents/MacOS/claude")],
            session_panes: vec![("oo-abc-1".into(), 100)],
        };
        let v = classify(&s, &[scope("oo-abc", "web")]);
        assert!(v.task.is_empty());
        assert!(v.other.is_empty());
    }

    #[test]
    fn classify_filters_system_and_self() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![
                l(1403, "*", 7000),
                l(1260, "*", 65403),
                l(1, "*", 7443),
                l(1741, "*", 8080),
            ],
            procs: vec![
                p(1403, 0, "/System/Library/CoreServices/ControlCenter.app/Contents/MacOS/ControlCenter"),
                p(1260, 0, "/usr/libexec/rapportd"),
                p(1, 0, "/Applications/openOrchestra.app/Contents/MacOS/openOrchestra"),
                p(1741, 0, "/Applications/OrbStack.app/Contents/MacOS/OrbStack"),
            ],
            session_panes: vec![],
        };
        let v = classify(&s, &[]);
        assert_eq!(v.other.len(), 1);
        assert_eq!(v.other[0].port, 8080);
        assert_eq!(v.other[0].name, "OrbStack");
    }

    #[test]
    fn classify_labels_members_only_for_multi_scope() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(201, "*", 4000), l(301, "*", 3000)],
            procs: vec![p(200, 0, "/bin/zsh"), p(201, 200, "/x/node"), p(300, 0, "/bin/zsh"), p(301, 300, "/x/node")],
            session_panes: vec![("oo-api-1".into(), 200), ("oo-web-1".into(), 300)],
        };
        let v = classify(&s, &[scope("oo-api", "api"), scope("oo-web", "web")]);
        let members: Vec<_> = v.task.iter().map(|r| (r.port, r.member.clone())).collect();
        assert_eq!(members, vec![(3000, Some("web".into())), (4000, Some("api".into()))]);
    }
}
