//! Listening TCP ports for the tools panel's Ports tab.
//!
//! One `lsof` call lists every listener on the Mac; `ps` supplies each
//! process's executable path and parent; the tmux pane table says which
//! process trees belong to which worktree. [`classify`] joins them into
//! "this task" and "other on this Mac". Everything but [`sample`] is pure so
//! the attribution is testable without a live machine.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

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
    pub containers: Vec<Container>,
    /// Each listener's working folder (`lsof -d cwd`).
    pub cwds: std::collections::HashMap<u32, PathBuf>,
}

/// A worktree (or coordinator) whose tmux sessions count as "this task".
#[derive(Clone, Debug)]
pub struct Scope {
    pub label: String,
    pub session_prefix: String,
    /// The worktree (or checkout) folder; a Compose container started from
    /// inside it belongs to this scope.
    pub path: Option<PathBuf>,
    /// The repo name, for matching containers by name when nothing else
    /// links them (`comicchat-ergo` → `comic-chat`).
    pub repo: Option<String>,
}

/// A running container with its published ports, from `docker ps`.
#[derive(Clone, Debug, PartialEq)]
pub struct Container {
    pub name: String,
    pub image: String,
    /// `(host_port, container_port)`, TCP only.
    pub published: Vec<(u16, u16)>,
    pub compose_project: Option<String>,
    pub compose_service: Option<String>,
    /// Where `docker compose up` ran.
    pub working_dir: Option<PathBuf>,
    /// Host folders bind-mounted into the container.
    pub binds: Vec<PathBuf>,
}

/// What a port row knows about the container behind it.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerInfo {
    /// Compose service and project (`db · urbital`), or the container name.
    pub label: String,
    pub container_port: u16,
    pub name: String,
    pub image: String,
    /// Placed in the task only because its name starts with the repo's: a
    /// guess, so the row says so.
    pub matched_by_name: bool,
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
    /// Set when the port is published by a container.
    pub container: Option<ContainerInfo>,
    /// The process's working folder, when known.
    pub cwd: Option<PathBuf>,
}

/// Which column the Ports tab sorts by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortField {
    #[default]
    Port,
    Name,
}

/// Ascending or descending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

/// How the Ports tab orders its rows. Persisted in prefs; either half absent
/// falls back to its default (port, ascending).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PortSort {
    #[serde(default)]
    pub field: SortField,
    #[serde(default)]
    pub order: SortOrder,
}

impl SortField {
    pub const ALL: [SortField; 2] = [SortField::Port, SortField::Name];

    pub fn label(self) -> &'static str {
        match self {
            SortField::Port => "Port",
            SortField::Name => "Name",
        }
    }
}

impl SortOrder {
    pub const ALL: [SortOrder; 2] = [SortOrder::Asc, SortOrder::Desc];

    pub fn label(self) -> &'static str {
        match self {
            SortOrder::Asc => "Ascending",
            SortOrder::Desc => "Descending",
        }
    }
}

impl PortRow {
    /// What the row is called: the container when there is one, else the
    /// process.
    pub fn display_name(&self) -> &str {
        self.container.as_ref().map(|c| c.label.as_str()).unwrap_or(&self.name)
    }
}

/// Order rows in place.
pub fn sort_rows(rows: &mut [PortRow], by: PortSort) {
    let flip = |o: std::cmp::Ordering| if by.order == SortOrder::Desc { o.reverse() } else { o };
    let tiebreak = |a: &PortRow, b: &PortRow| (a.port, a.pid).cmp(&(b.port, b.pid));
    match by.field {
        SortField::Port => rows.sort_by(|a, b| flip(tiebreak(a, b))),
        // Equal names (several `node` servers) read low port first either way.
        SortField::Name => rows.sort_by(|a, b| {
            let (x, y) = (a.display_name().to_lowercase(), b.display_name().to_lowercase());
            flip(x.cmp(&y)).then_with(|| tiebreak(a, b))
        }),
    }
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

/// The `docker ps --format` template [`parse_docker_ps`] reads: tab-separated
/// name, image, ports, compose project, compose service, compose working dir,
/// mounts (volume names and bind sources, comma-separated; needs `--no-trunc`).
pub const DOCKER_PS_FORMAT: &str = "{{.Names}}\t{{.Image}}\t{{.Ports}}\t{{.Label \"com.docker.compose.project\"}}\t{{.Label \"com.docker.compose.service\"}}\t{{.Label \"com.docker.compose.project.working_dir\"}}\t{{.Mounts}}";

/// Parse `docker ps --format DOCKER_PS_FORMAT` output.
pub fn parse_docker_ps(out: &str) -> Vec<Container> {
    let opt = |v: Option<&str>| v.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
    out.lines()
        .filter_map(|line| {
            let mut f = line.split('\t');
            let name = f.next()?.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let image = f.next().unwrap_or("").trim().to_string();
            let mut published: Vec<(u16, u16)> = Vec::new();
            for mapping in f.next().unwrap_or("").split(", ").filter_map(parse_published) {
                for pair in mapping {
                    if !published.contains(&pair) {
                        published.push(pair);
                    }
                }
            }
            Some(Container {
                name,
                image,
                published,
                compose_project: opt(f.next()),
                compose_service: opt(f.next()),
                working_dir: opt(f.next()).map(PathBuf::from),
                // Bind sources are absolute paths; anything else is a volume name.
                binds: f
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .map(str::trim)
                    .filter(|m| m.starts_with('/'))
                    .map(PathBuf::from)
                    .collect(),
            })
        })
        .collect()
}

/// One `docker ps` port entry: `0.0.0.0:5433->5432/tcp`, `[::]:80->80/tcp`,
/// `0.0.0.0:8000-8002->9000-9002/tcp`. Unpublished (`6667/tcp`) and UDP
/// entries give `None`.
fn parse_published(entry: &str) -> Option<Vec<(u16, u16)>> {
    let entry = entry.trim().strip_suffix("/tcp")?;
    let (host, inner) = entry.split_once("->")?;
    let (_, host) = host.rsplit_once(':')?;
    let range = |r: &str| -> Option<(u16, u16)> {
        match r.split_once('-') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => {
                let p = r.parse().ok()?;
                Some((p, p))
            }
        }
    };
    let (h0, h1) = range(host)?;
    let (i0, _) = range(inner)?;
    if h1 < h0 {
        return None;
    }
    Some((h0..=h1).map(|h| (h, i0 + (h - h0))).collect())
}

/// Path fragments of the processes that publish container ports on the host
/// (OrbStack, Docker Desktop, Colima/Lima, Podman, Rancher Desktop). `docker
/// ps` only runs when one of them is listening.
const ENGINE_HINTS: &[&str] = &["OrbStack", "Docker.app", "com.docker", "colima", "lima", "podman", "Rancher"];

/// Parse `lsof -a -p <pids> -d cwd -Fpn` output into pid → working folder.
pub fn parse_lsof_cwd(out: &str) -> std::collections::HashMap<u32, PathBuf> {
    let mut pid: Option<u32> = None;
    let mut cwds = std::collections::HashMap::new();
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().ok();
        } else if let (Some(rest), Some(p)) = (line.strip_prefix('n'), pid) {
            cwds.insert(p, PathBuf::from(rest));
        }
    }
    cwds
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
            .filter(|(name, _)| in_scope(name, &sc.session_prefix))
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
    // host port -> (container, port inside it), only for a port exactly one
    // container publishes. Two containers can publish one port number on
    // different host addresses, and the single engine process holding both
    // sockets can't say which is which: naming either would be a guess.
    let mut by_port: std::collections::HashMap<u16, Vec<(&Container, u16)>> = Default::default();
    for c in &s.containers {
        for &(h, i) in &c.published {
            by_port.entry(h).or_default().push((c, i));
        }
    }
    let published: std::collections::HashMap<u16, (&Container, u16)> = by_port
        .into_iter()
        .filter_map(|(h, owners)| match owners.as_slice() {
            [only] => Some((h, *only)),
            _ => None,
        })
        .collect();
    let mut view = PortsView::default();
    for ((port, pid), mut addrs) in grouped {
        addrs.sort();
        let path = path_of(pid).unwrap_or("");
        let name = if path.is_empty() { "?".to_string() } else { basename(path).to_string() };
        // An agent CLI's own listener (Claude Code's IDE bridge, …) is never the
        // user's app, in this task or any other session.
        if agents.contains(name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let member_of = |label: &str| (scopes.len() > 1).then(|| label.to_string());
        let cwd = s.cwds.get(&pid).cloned();
        if let Some(label) = owner.get(&pid) {
            let member = member_of(label);
            view.task.push(PortRow { port, addrs, pid, name, member, container: None, cwd });
            continue;
        }
        if pid == s.app_pid || SYSTEM_PREFIXES.iter().any(|p| path.starts_with(p)) {
            continue;
        }
        // Container metadata only for a port an engine holds: a native server
        // on the same port number (say [::1]:8080 beside a container's
        // 127.0.0.1:8080) is not that container.
        let is_engine = ENGINE_HINTS.iter().any(|h| path.contains(h));
        let (container, scope) = match published.get(&port).filter(|_| is_engine) {
            Some(&(c, container_port)) => {
                let label = match (&c.compose_service, &c.compose_project) {
                    (Some(svc), Some(proj)) => format!("{svc} · {proj}"),
                    _ => c.name.clone(),
                };
                let (scope, matched_by_name) = container_scope(c, scopes);
                let info = ContainerInfo {
                    label,
                    container_port,
                    name: c.name.clone(),
                    image: c.image.clone(),
                    matched_by_name,
                };
                (Some(info), scope)
            }
            // Not in a pane's tree (started in the background, its parent
            // gone), but running from inside a scope's folder. An engine's
            // folder says nothing about its containers, so engines never
            // match this way.
            None if !is_engine => {
                let scope = cwd.as_ref().and_then(|d| {
                    scopes.iter().find(|sc| sc.path.as_ref().is_some_and(|p| d.starts_with(p)))
                });
                (None, scope)
            }
            None => (None, None),
        };
        match scope {
            Some(sc) => {
                let member = member_of(&sc.label);
                view.task.push(PortRow { port, addrs, pid, name, member, container, cwd });
            }
            None => view.other.push(PortRow { port, addrs, pid, name, member: None, container, cwd }),
        }
    }
    view
}

/// Which scope a container belongs to, and whether that was only a name
/// guess. In order of trust: the Compose folder (decisive either way: a Compose
/// container from elsewhere is never re-homed by name), a bind-mounted folder
/// inside the scope, then a container name starting with the repo name.
fn container_scope<'a>(c: &Container, scopes: &'a [Scope]) -> (Option<&'a Scope>, bool) {
    // Path::starts_with is per component: `feat-xy` is not inside `feat-x`.
    let inside = |dir: &PathBuf| {
        scopes.iter().find(|sc| sc.path.as_ref().is_some_and(|p| dir.starts_with(p)))
    };
    if let Some(wd) = &c.working_dir {
        return (inside(wd), false);
    }
    if let Some(sc) = c.binds.iter().find_map(inside) {
        return (Some(sc), false);
    }
    let name = squash(&c.name);
    let by_name = scopes.iter().find(|sc| {
        sc.repo
            .as_deref()
            .map(squash)
            .is_some_and(|repo| repo.len() >= MIN_NAME_MATCH && name.starts_with(&repo))
    });
    (by_name, by_name.is_some())
}

/// Shorter repo names than this never match by name: `ui` or `db` would claim
/// half the containers on the machine.
const MIN_NAME_MATCH: usize = 4;

/// Lowercase alphanumerics only, so `comic-chat` and `comicchat` compare equal.
fn squash(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

/// Whether a tmux session belongs to the scope with this prefix. Session
/// names are `prefix` + a numeric tab id (`tmux::session_name`); a bare
/// `starts_with` also claimed a longer key's sessions (`main:api` took
/// `main:api_admin`'s, both slugging to `harness_main-api_…`).
fn in_scope(session: &str, prefix: &str) -> bool {
    session
        .strip_prefix(prefix)
        .is_some_and(|tab| !tab.is_empty() && tab.bytes().all(|b| b.is_ascii_digit()))
}

/// The tab label: the count only while the tab is visible, since sampling
/// stops when it isn't and a hidden tab's count would be stale.
pub fn tab_label(task_count: usize, visible: bool) -> String {
    if visible && task_count > 0 {
        format!("Ports · {task_count}")
    } else {
        "Ports".to_string()
    }
}

/// The browser URL for a row. `localhost` when the server accepts loopback
/// (bound to every interface or to a loopback address); otherwise the one
/// address it is bound to, since `localhost` would miss it or reach a
/// different server on the same port.
pub fn url_for(row: &PortRow) -> String {
    let port = row.port;
    let loopback_ok = row
        .addrs
        .iter()
        .any(|a| matches!(a.as_str(), "*" | "127.0.0.1" | "::1" | "localhost"));
    match row.addrs.first() {
        Some(addr) if !loopback_ok && addr.contains(':') => format!("http://[{addr}]:{port}"),
        Some(addr) if !loopback_ok => format!("http://{addr}:{port}"),
        _ => format!("http://localhost:{port}"),
    }
}

/// Run `lsof`, `ps` and (when available) `tmux list-panes`. Blocking: call
/// it off the UI thread. A tool that fails or is missing contributes nothing.
/// Every external command a sample runs gets this long.
const SCAN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Run `lsof`, `ps`, and (when available) `tmux list-panes`, each bounded by
/// [`SCAN_TIMEOUT`]. Blocking: call it off the UI thread. `None` when the
/// listener scan or the process table didn't come back in time (an `lsof`
/// stuck on an unreachable network mount, say): the caller keeps its last
/// sample and the next tick retries. An empty answer is still `Some`.
pub fn sample(has_tmux: bool) -> Option<PortsSample> {
    sample_using(|cmd, args| run_bounded(cmd, args, SCAN_TIMEOUT), has_tmux)
}

/// [`sample`] with the command runner passed in, so tests can play a stalled
/// or empty scan. `run` returns stdout, or `None` on timeout or spawn failure.
pub fn sample_using(
    run: impl Fn(&str, &[&str]) -> Option<String>,
    has_tmux: bool,
) -> Option<PortsSample> {
    // lsof exits 1 when nothing matches; its stdout is still the answer.
    let listeners = parse_lsof(&run("lsof", &["-nP", "-w", "-iTCP", "-sTCP:LISTEN", "-F", "pcn"])?);
    let procs = parse_ps_paths(&run("ps", &["-axo", "pid=,ppid=,comm="])?);
    let session_panes = if has_tmux { crate::tmux::list_session_panes() } else { Vec::new() };
    let engine_listening = listeners.iter().any(|l| {
        procs
            .iter()
            .find(|p| p.pid == l.pid)
            .is_some_and(|p| ENGINE_HINTS.iter().any(|h| p.path.contains(h)))
    });
    // Every listener's working folder, in one call. Enrichment only: a
    // timeout here leaves the rows without folders rather than dropping them.
    let mut pids: Vec<String> = listeners.iter().map(|l| l.pid.to_string()).collect();
    pids.sort();
    pids.dedup();
    let cwds = if pids.is_empty() {
        Default::default()
    } else {
        let list = pids.join(",");
        run("lsof", &["-a", "-w", "-p", &list, "-d", "cwd", "-Fpn"])
            .map(|out| parse_lsof_cwd(&out))
            .unwrap_or_default()
    };
    let containers = if engine_listening {
        run("docker", &["ps", "--no-trunc", "--format", DOCKER_PS_FORMAT])
            .map(|out| parse_docker_ps(&out))
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    Some(PortsSample { app_pid: std::process::id(), listeners, procs, session_panes, containers, cwds })
}

/// Run a command and return its stdout whatever its exit status, giving up
/// (and killing it) after
/// `timeout`: a stopped Docker daemon can leave `docker ps` waiting.
fn run_bounded(cmd: &str, args: &[&str], timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = std::process::Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on its own thread so a large output can't fill the pipe
    // while we wait.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.read_to_string(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            // Exit status is ignored: lsof exits 1 when one of the pids has
            // gone, yet still prints the rest; a failed `docker ps` prints
            // nothing, which parses to nothing.
            Ok(Some(_)) => return reader.join().ok(),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
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
        Scope { label: label.into(), session_prefix: prefix.into(), path: None, repo: None }
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

    const DOCKER_PS: &str = "urbital-db-1\tpostgres:16\t0.0.0.0:5433->5432/tcp, [::]:5433->5432/tcp\turbital\tdb\t/wt/urbital/feat-x\n\
comicchat-ergo\tergo:latest\t6667/tcp, 127.0.0.1:8067->8067/tcp, 6697/tcp, 127.0.0.1:8089->8089/tcp\t\t\t\n\
ranges-1\tapp\t0.0.0.0:8000-8002->9000-9002/tcp, 0.0.0.0:53->53/udp\t\t\t\n\
quiet-1\tbusybox\t\t\t\t\n";

    fn container(name: &str, host: u16, inner: u16, wd: Option<&str>) -> Container {
        Container {
            name: name.into(),
            image: "img".into(),
            published: vec![(host, inner)],
            compose_project: wd.map(|_| "urbital".into()),
            compose_service: wd.map(|_| "db".into()),
            working_dir: wd.map(PathBuf::from),
            binds: vec![],
        }
    }

    fn engine_sample(containers: Vec<Container>, ports: &[u16]) -> PortsSample {
        PortsSample {
            app_pid: 1,
            listeners: ports.iter().map(|&p| l(41084, "*", p)).collect(),
            procs: vec![p(41084, 0, "/Applications/OrbStack.app/Contents/MacOS/OrbStack")],
            session_panes: vec![],
            containers,
            cwds: Default::default(),
        }
    }

    fn repo_scope(repo: &str, path: &str) -> Scope {
        Scope {
            label: repo.into(),
            session_prefix: "oo-x".into(),
            path: Some(PathBuf::from(path)),
            repo: Some(repo.into()),
        }
    }

    #[test]
    fn parse_docker_ps_keeps_bind_sources_not_volume_names() {
        let out = "a\timg\t0.0.0.0:1->1/tcp\t\t\t\t/wt/comic-chat/x/data,named_vol\n";
        assert_eq!(parse_docker_ps(out)[0].binds, vec![PathBuf::from("/wt/comic-chat/x/data")]);
    }

    #[test]
    fn classify_puts_a_container_with_a_bind_mount_from_the_task_in_the_task() {
        let mut c = container("whatever-1", 7000, 7000, None);
        c.binds = vec![PathBuf::from("/wt/comic-chat/x/data")];
        let v = classify(&engine_sample(vec![c], &[7000]), &[repo_scope("comic-chat", "/wt/comic-chat/x")]);
        assert_eq!(v.task.len(), 1);
        assert!(!v.task[0].container.as_ref().unwrap().matched_by_name);
    }

    #[test]
    fn classify_matches_a_plain_container_by_repo_name_and_says_so() {
        let c = container("comicchat-ergo", 8067, 8067, None);
        let v = classify(&engine_sample(vec![c], &[8067]), &[repo_scope("comic-chat", "/wt/comic-chat/x")]);
        assert_eq!(v.task.len(), 1);
        assert!(v.task[0].container.as_ref().unwrap().matched_by_name);
    }

    #[test]
    fn name_matching_never_overrides_compose_or_short_names() {
        // A compose container from elsewhere stays where its folder says.
        let compose = container("comicchat-db-1", 5432, 5432, Some("/elsewhere"));
        // A two-letter repo name would match far too much.
        let short = container("ui-thing", 3000, 3000, None);
        let v = classify(
            &engine_sample(vec![compose, short], &[5432, 3000]),
            &[repo_scope("comic-chat", "/wt/comic-chat/x"), repo_scope("ui", "/wt/ui/x")],
        );
        assert!(v.task.is_empty(), "{:?}", v.task);
        assert_eq!(v.other.len(), 2);
    }

    #[test]
    fn parse_docker_ps_reads_compose_labels_and_published_ports() {
        let cs = parse_docker_ps(DOCKER_PS);
        assert_eq!(cs.len(), 4);
        assert_eq!(cs[0].name, "urbital-db-1");
        assert_eq!(cs[0].image, "postgres:16");
        assert_eq!(cs[0].published, vec![(5433, 5432)], "v4 and v6 bindings are one mapping");
        assert_eq!(cs[0].compose_project.as_deref(), Some("urbital"));
        assert_eq!(cs[0].compose_service.as_deref(), Some("db"));
        assert_eq!(cs[0].working_dir, Some(PathBuf::from("/wt/urbital/feat-x")));
        assert_eq!(cs[1].published, vec![(8067, 8067), (8089, 8089)], "unpublished ports skipped");
        assert_eq!(cs[1].compose_project, None, "empty label is None");
        assert_eq!(cs[2].published, vec![(8000, 9000), (8001, 9001), (8002, 9002)], "ranges expand, udp skipped");
        assert!(cs[3].published.is_empty());
    }

    #[test]
    fn classify_names_the_container_behind_an_engine_port() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(41084, "*", 8067), l(41084, "127.0.0.1", 32222)],
            procs: vec![p(41084, 0, "/Applications/OrbStack.app/Contents/MacOS/OrbStack")],
            session_panes: vec![],
            containers: vec![container("comicchat-ergo", 8067, 6667, None)],
            cwds: Default::default(),
        };
        let v = classify(&s, &[]);
        assert_eq!(v.other.len(), 2);
        let c = v.other[0].container.as_ref().expect("8067 is a container port");
        assert_eq!(c.label, "comicchat-ergo");
        assert_eq!(c.container_port, 6667);
        assert_eq!(v.other[1].container, None, "engine's own port stays plain");
    }

    #[test]
    fn a_native_server_on_a_containers_port_keeps_its_own_identity() {
        // A container publishes 127.0.0.1:8080 (held by OrbStack) while an
        // unrelated node listens on [::1]:8080. Only the engine's row is the
        // container.
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(41084, "127.0.0.1", 8080), l(7, "::1", 8080)],
            procs: vec![
                p(41084, 0, "/Applications/OrbStack.app/Contents/MacOS/OrbStack"),
                p(7, 0, "/opt/homebrew/bin/node"),
            ],
            session_panes: vec![],
            containers: vec![container("web-1", 8080, 80, None)],
            cwds: Default::default(),
        };
        let v = classify(&s, &[]);
        let node = v.other.iter().find(|r| r.pid == 7).unwrap();
        assert_eq!(node.container, None, "node is not the container");
        let engine = v.other.iter().find(|r| r.pid == 41084).unwrap();
        assert!(engine.container.is_some());
    }

    #[test]
    fn a_port_two_containers_publish_is_not_given_either_name() {
        // a on 127.0.0.1:8080 and b on 192.168.1.5:8080: one engine holds
        // both sockets, so the port can't say which container it is.
        let s = engine_sample(
            vec![container("a-1", 8080, 80, None), container("b-1", 8080, 81, None)],
            &[8080],
        );
        let v = classify(&s, &[]);
        assert_eq!(v.other.len(), 1);
        assert_eq!(v.other[0].container, None);
    }

    #[test]
    fn classify_moves_a_compose_container_into_its_task() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(41084, "*", 5433), l(41084, "*", 8081)],
            procs: vec![p(41084, 0, "/Applications/OrbStack.app/Contents/MacOS/OrbStack")],
            session_panes: vec![],
            containers: vec![
                container("urbital-db-1", 5433, 5432, Some("/wt/urbital/feat-x")),
                container("other-db-1", 8081, 8080, Some("/wt/urbital/feat-xy")),
            ],
            cwds: Default::default(),
        };
        let sc = Scope {
            label: "urbital".into(),
            session_prefix: "oo-x".into(),
            path: Some(PathBuf::from("/wt/urbital/feat-x")),
            repo: None,
        };
        let v = classify(&s, &[sc]);
        assert_eq!(v.task.len(), 1);
        assert_eq!(v.task[0].port, 5433);
        assert_eq!(v.task[0].container.as_ref().unwrap().label, "db · urbital");
        assert_eq!(v.other.len(), 1, "a sibling folder with a shared name prefix is not inside");
        assert_eq!(v.other[0].port, 8081);
    }

    fn row(port: u16, name: &str, container: Option<&str>) -> PortRow {
        PortRow {
            port,
            addrs: vec!["*".into()],
            pid: port as u32,
            name: name.into(),
            member: None,
            container: container.map(|c| ContainerInfo {
                label: c.into(),
                container_port: port,
                name: c.into(),
                image: "img".into(),
                matched_by_name: false,
            }),
            cwd: None,
        }
    }

    fn ports(rows: &[PortRow]) -> Vec<u16> {
        rows.iter().map(|r| r.port).collect()
    }

    fn by(field: SortField, order: SortOrder) -> PortSort {
        PortSort { field, order }
    }

    #[test]
    fn sort_rows_by_port_both_ways() {
        let mut rows = vec![row(5173, "node", None), row(3000, "node", None), row(8080, "ruby", None)];
        sort_rows(&mut rows, by(SortField::Port, SortOrder::Desc));
        assert_eq!(ports(&rows), vec![8080, 5173, 3000]);
        sort_rows(&mut rows, by(SortField::Port, SortOrder::Asc));
        assert_eq!(ports(&rows), vec![3000, 5173, 8080]);
    }

    #[test]
    fn sort_rows_by_name_uses_the_container_and_ignores_case() {
        let mut rows = vec![
            row(9000, "Zeta", None),
            row(5433, "OrbStack Helper", Some("db · urbital")),
            row(4000, "node", None),
            row(3000, "node", None),
        ];
        sort_rows(&mut rows, by(SortField::Name, SortOrder::Asc));
        // db (container) < node (3000, then 4000: port breaks ties) < Zeta
        assert_eq!(ports(&rows), vec![5433, 3000, 4000, 9000]);
        sort_rows(&mut rows, by(SortField::Name, SortOrder::Desc));
        // Names reverse; equal names still read low port first.
        assert_eq!(ports(&rows), vec![9000, 3000, 4000, 5433]);
    }

    #[test]
    fn port_sort_persists_and_defaults_to_port_ascending() {
        let json = serde_json::to_string(&by(SortField::Name, SortOrder::Desc)).unwrap();
        assert_eq!(json, r#"{"field":"name","order":"desc"}"#);
        assert_eq!(serde_json::from_str::<PortSort>(&json).unwrap(), by(SortField::Name, SortOrder::Desc));
        assert_eq!(serde_json::from_str::<PortSort>(r#"{"field":"name"}"#).unwrap().order, SortOrder::Asc);
        assert_eq!(PortSort::default(), by(SortField::Port, SortOrder::Asc));
    }

    #[test]
    fn url_uses_localhost_only_when_the_server_accepts_loopback() {
        let at = |addrs: &[&str]| PortRow {
            addrs: addrs.iter().map(|a| a.to_string()).collect(),
            ..row(8080, "node", None)
        };
        assert_eq!(url_for(&at(&["*"])), "http://localhost:8080");
        assert_eq!(url_for(&at(&["127.0.0.1"])), "http://localhost:8080");
        assert_eq!(url_for(&at(&["::1"])), "http://localhost:8080");
        assert_eq!(url_for(&at(&["192.168.1.20"])), "http://192.168.1.20:8080");
        assert_eq!(url_for(&at(&["fe80::1"])), "http://[fe80::1]:8080");
    }

    #[test]
    fn a_timed_out_listener_scan_is_no_sample_but_an_empty_one_is() {
        // `None` from the runner = timed out or failed to start.
        let timed_out = |cmd: &str, _: &[&str]| (cmd != "lsof").then(String::new);
        assert!(sample_using(timed_out, false).is_none(), "keep the last sample; retry next tick");
        let empty = |_: &str, _: &[&str]| Some(String::new());
        let s = sample_using(empty, false).expect("an empty scan is a real answer");
        assert!(s.listeners.is_empty());
    }

    #[test]
    fn tab_label_counts_only_while_visible() {
        assert_eq!(tab_label(2, true), "Ports · 2");
        assert_eq!(tab_label(2, false), "Ports", "a hidden tab's sample is stale");
        assert_eq!(tab_label(0, true), "Ports");
    }

    #[test]
    fn parse_lsof_cwd_maps_pid_to_folder() {
        let out = "p93830\nfcwd\nn/Users/omar/projects/comic-chat\np1260\nfcwd\nn/\np7\nfcwd\nn/a folder/with spaces\n";
        let m = parse_lsof_cwd(out);
        assert_eq!(m.get(&93830), Some(&PathBuf::from("/Users/omar/projects/comic-chat")));
        assert_eq!(m.get(&1260), Some(&PathBuf::from("/")));
        assert_eq!(m.get(&7), Some(&PathBuf::from("/a folder/with spaces")));
    }

    fn cwd_sample(pid: u32, path: &str, cwd: &str, port: u16) -> PortsSample {
        PortsSample {
            app_pid: 1,
            listeners: vec![l(pid, "*", port)],
            // Parent 1: orphaned to launchd, so no pane tree reaches it.
            procs: vec![p(pid, 1, path)],
            session_panes: vec![],
            containers: vec![],
            cwds: [(pid, PathBuf::from(cwd))].into_iter().collect(),
        }
    }

    #[test]
    fn classify_puts_an_orphaned_server_in_the_task_its_folder_is_in() {
        let s = cwd_sample(93830, "/opt/homebrew/bin/bun", "/Users/omar/projects/comic-chat/apps/web", 3000);
        let v = classify(&s, &[repo_scope("comic-chat", "/Users/omar/projects/comic-chat")]);
        assert_eq!(v.task.len(), 1, "{v:?}");
        assert_eq!(v.task[0].cwd, Some(PathBuf::from("/Users/omar/projects/comic-chat/apps/web")));
    }

    #[test]
    fn classify_ignores_an_engine_s_working_folder() {
        // OrbStack happens to run from some project folder; that says nothing.
        let s = cwd_sample(41084, "/Applications/OrbStack.app/Contents/MacOS/OrbStack", "/Users/omar/projects/comic-chat/ios", 32222);
        let v = classify(&s, &[repo_scope("comic-chat", "/Users/omar/projects/comic-chat")]);
        assert!(v.task.is_empty());
        assert_eq!(v.other.len(), 1);
    }

    #[test]
    fn classify_keeps_a_server_from_a_sibling_folder_out() {
        let s = cwd_sample(5, "/x/node", "/Users/omar/projects/comic-chat-old", 4000);
        let v = classify(&s, &[repo_scope("comic-chat", "/Users/omar/projects/comic-chat")]);
        assert!(v.task.is_empty());
        assert_eq!(v.other[0].cwd, Some(PathBuf::from("/Users/omar/projects/comic-chat-old")));
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
            containers: vec![],
            cwds: Default::default(),
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
                container: None,
                cwd: None,
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
            session_panes: vec![("oo-abc_1".into(), 100)],
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[scope("oo-abc_", "web")]);
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
            session_panes: vec![("oo-abc_1".into(), 100)],
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[scope("oo-abc_", "web")]);
        assert!(v.task.is_empty());
        assert!(v.other.is_empty());
    }

    #[test]
    fn classify_hides_agent_ports_outside_the_task_too() {
        // Other sessions' Claude Code listeners would otherwise fill "Other".
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(900, "127.0.0.1", 50465), l(901, "*", 9000)],
            procs: vec![p(900, 0, "/x/claude.app/Contents/MacOS/claude"), p(901, 0, "/x/node")],
            session_panes: vec![],
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[]);
        let ports: Vec<u16> = v.other.iter().map(|r| r.port).collect();
        assert_eq!(ports, vec![9000]);
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
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[]);
        assert_eq!(v.other.len(), 1);
        assert_eq!(v.other[0].port, 8080);
        assert_eq!(v.other[0].name, "OrbStack");
    }

    /// Live: a real child listener shows up under "other" via lsof + ps.
    /// Ignored by default (needs lsof and python3 on the machine).
    #[test]
    #[ignore]
    fn live_sample_finds_a_child_server() {
        let mut child = std::process::Command::new("python3")
            .args(["-m", "http.server", "18765", "--bind", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut found = None;
        for _ in 0..30 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let v = classify(&sample(false).expect("scan finished"), &[]);
            if let Some(r) = v.other.iter().find(|r| r.port == 18765) {
                found = Some(r.clone());
                break;
            }
        }
        // Kill and reap before any assertion can panic, so no zombie is left.
        let _ = child.kill();
        let _ = child.wait();
        let row = found.expect("port 18765 not found");
        assert_eq!(row.pid, pid);
        assert_eq!(row.addrs, vec!["127.0.0.1".to_string()]);
        assert!(row.name.to_ascii_lowercase().starts_with("python"), "{}", row.name);
    }

    /// Live: every published container port on this machine is labelled.
    /// Ignored by default (needs a running Docker engine with containers).
    #[test]
    #[ignore]
    fn live_sample_labels_running_containers() {
        let s = sample(false).expect("scan finished");
        assert!(!s.containers.is_empty(), "no containers seen; is an engine running?");
        let v = classify(&s, &[]);
        for c in &s.containers {
            for (host, inner) in &c.published {
                let row = v.other.iter().find(|r| r.port == *host);
                let row = row.unwrap_or_else(|| panic!("{}:{host} not listed", c.name));
                let info = row.container.as_ref().unwrap_or_else(|| panic!(":{host} unlabelled"));
                assert_eq!(info.container_port, *inner);
                eprintln!(":{host} -> {} (→ {inner}) via {}", info.label, row.name);
            }
        }
    }

    #[test]
    fn a_session_of_a_longer_key_is_not_this_scopes() {
        // `main:api` and `main:api_admin` share the stem `harness_main-api_`;
        // only a numeric tab id may follow a scope's own prefix.
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(11, "*", 3000), l(21, "*", 4000)],
            procs: vec![p(10, 0, "/bin/zsh"), p(11, 10, "/x/node"), p(20, 0, "/bin/zsh"), p(21, 20, "/x/node")],
            session_panes: vec![("harness_main-api_7".into(), 10), ("harness_main-api_admin_7".into(), 20)],
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[scope("harness_main-api_", "api")]);
        assert_eq!(ports(&v.task), vec![3000]);
        assert_eq!(ports(&v.other), vec![4000]);
    }

    #[test]
    fn classify_labels_members_only_for_multi_scope() {
        let s = PortsSample {
            app_pid: 1,
            listeners: vec![l(201, "*", 4000), l(301, "*", 3000)],
            procs: vec![p(200, 0, "/bin/zsh"), p(201, 200, "/x/node"), p(300, 0, "/bin/zsh"), p(301, 300, "/x/node")],
            session_panes: vec![("oo-api_1".into(), 200), ("oo-web_1".into(), 300)],
            containers: vec![],
            cwds: Default::default(),
        };
        let v = classify(&s, &[scope("oo-api_", "api"), scope("oo-web_", "web")]);
        let members: Vec<_> = v.task.iter().map(|r| (r.port, r.member.clone())).collect();
        assert_eq!(members, vec![(3000, Some("web".into())), (4000, Some("api".into()))]);
    }
}
