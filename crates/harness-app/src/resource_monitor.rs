//! How much of the machine openOrchestra is using — the number shown atop the
//! sidebar, and the per-worktree breakdown behind its hover.
//!
//! The footprint splits in two:
//!   * **base** — the GUI process and its direct children (non-tmux panes).
//!     Shared; it can't be pinned to any one worktree.
//!   * **per-worktree** — the agents and shells a worktree's tmux sessions run.
//!     Those live under the tmux *server*, a separate process tree, enumerated
//!     per session and matched back to a worktree by session-name prefix
//!     (`tmux::session_prefix`).
//!
//! `aggregate` is a pure function over already-collected inputs so the whole
//! accounting is unit-testable without a live tmux server, a GUI, or sysinfo.
//! The collector that feeds it real samples lives in `spawn_sampler` below.

use std::collections::{HashMap, HashSet, VecDeque};

use sysinfo::{ProcessesToUpdate, System};

/// CPU percent (100 = one core saturated; a subtree spanning cores can exceed
/// 100) and resident memory in bytes.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub struct Usage {
    pub cpu_pct: f32,
    pub rss_bytes: u64,
}

impl Usage {
    fn add(&mut self, other: Usage) {
        self.cpu_pct += other.cpu_pct;
        self.rss_bytes += other.rss_bytes;
    }
}

/// One process, flattened out of sysinfo so aggregation needs nothing from it.
#[derive(Clone, Copy, Debug)]
pub struct Proc {
    pub pid: u32,
    pub parent: Option<u32>,
    pub cpu_pct: f32,
    pub rss_bytes: u64,
}

/// A worktree (or main checkout) that sessions can be attributed to.
#[derive(Clone, Debug)]
pub struct Attributable {
    /// The name shown in the hover breakdown row.
    pub label: String,
    /// `tmux::session_prefix(key)` — the stem every session for this key shares.
    /// A session is this worktree's when its name starts with this.
    pub session_prefix: String,
}

/// One row of the hover breakdown.
#[derive(Clone, Debug, PartialEq)]
pub struct WorktreeUsage {
    pub label: String,
    pub usage: Usage,
}

/// A full accounting for one sample: the total shown on top, its base-app
/// share, and one entry per attributable worktree. By construction
/// `total == base + Σ per_worktree`.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub total: Usage,
    pub base: Usage,
    pub per_worktree: Vec<WorktreeUsage>,
}

/// Attribute every process to base-app or a worktree and total it up.
///
/// A pid is counted at most once across the whole snapshot (shared `visited`),
/// so a process reachable two ways — or a pane that happens to sit under the
/// GUI — never double-counts. Worktree subtrees are claimed first; whatever of
/// the app's own subtree is left over is the base.
pub fn aggregate(
    procs: &[Proc],
    app_pid: u32,
    session_panes: &[(String, u32)],
    worktrees: &[Attributable],
) -> Snapshot {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(parent) = p.parent {
            children.entry(parent).or_default().push(p.pid);
        }
    }

    let mut visited: HashSet<u32> = HashSet::new();
    let subtree = |root: u32, visited: &mut HashSet<u32>| -> Usage {
        let mut usage = Usage::default();
        let mut stack = vec![root];
        while let Some(pid) = stack.pop() {
            if !visited.insert(pid) {
                continue;
            }
            if let Some(p) = by_pid.get(&pid) {
                usage.add(Usage { cpu_pct: p.cpu_pct, rss_bytes: p.rss_bytes });
            }
            if let Some(kids) = children.get(&pid) {
                stack.extend(kids.iter().copied());
            }
        }
        usage
    };

    // Worktree buckets first, so anything they claim is excluded from base.
    let mut per_worktree = Vec::with_capacity(worktrees.len());
    for w in worktrees {
        let mut usage = Usage::default();
        for (session, pane_pid) in session_panes {
            if session.starts_with(&w.session_prefix) {
                usage.add(subtree(*pane_pid, &mut visited));
            }
        }
        per_worktree.push(WorktreeUsage { label: w.label.clone(), usage });
    }

    let base = subtree(app_pid, &mut visited);

    let mut total = base;
    for w in &per_worktree {
        total.add(w.usage);
    }
    Snapshot { total, base, per_worktree }
}

/// One tick's raw inputs, collected off the UI thread. Turned into a
/// [`Snapshot`] by `HarnessWorkspace::apply_resource`, which knows the current
/// worktree list, so this side stays free of app state.
pub struct RawSample {
    pub app_pid: u32,
    pub procs: Vec<Proc>,
    pub session_panes: Vec<(String, u32)>,
}

/// Refresh process stats and read the tmux pane table. Blocking (a sysinfo
/// refresh plus one `tmux` subprocess) — call it on a background thread. The
/// `System` is reused across ticks so CPU percentages are real deltas.
pub fn sample_once(sys: &mut System, has_tmux: bool) -> RawSample {
    sys.refresh_processes(ProcessesToUpdate::All, true);
    let procs = sys
        .processes()
        .iter()
        .map(|(pid, p)| Proc {
            pid: pid.as_u32(),
            parent: p.parent().map(|pp| pp.as_u32()),
            cpu_pct: p.cpu_usage(),
            rss_bytes: p.memory(),
        })
        .collect();
    let session_panes = if has_tmux {
        crate::tmux::list_session_panes()
    } else {
        Vec::new()
    };
    RawSample { app_pid: std::process::id(), procs, session_panes }
}

/// A process reduced to what agent-liveness needs: its pid, parent, and a
/// normalized command name (lowercased basename, leading `-` login marker
/// stripped).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: Option<u32>,
    pub comm: String,
}

/// Command basenames that count as "just a shell" — a pane sitting at one of
/// these, with no other descendant, means the agent has exited to the
/// `exec $SHELL` fallback and is gone.
const SHELL_COMMS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "csh", "tcsh", "ksh", "login", "tmux",
];

fn is_shell(comm: &str) -> bool {
    SHELL_COMMS.contains(&comm)
}

/// Parse `ps -axo pid=,ppid=,comm=` output into [`ProcInfo`]s, normalizing the
/// command to a lowercased basename without a leading login-shell `-`.
pub fn parse_ps(output: &str) -> Vec<ProcInfo> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid: u32 = parts.next()?.parse().ok()?;
            let ppid: u32 = parts.next()?.parse().ok()?;
            // comm is the last `=` field; rejoin in case a path carries spaces.
            let raw = parts.collect::<Vec<_>>().join(" ");
            if raw.is_empty() {
                return None;
            }
            let base = raw.rsplit('/').next().unwrap_or(&raw);
            let comm = base.trim_start_matches('-').to_ascii_lowercase();
            Some(ProcInfo { pid, parent: (ppid != 0).then_some(ppid), comm })
        })
        .collect()
}

/// Whether an agent — any non-shell process — is alive anywhere in the subtrees
/// rooted at `roots` (roots included). A subtree of only shells, or roots that
/// no longer exist, means no agent is running there.
pub fn agent_running_in(procs: &[ProcInfo], roots: &[u32]) -> bool {
    let by_pid: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.parent {
            children.entry(pp).or_default().push(p.pid);
        }
    }
    let mut visited: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if !visited.insert(pid) {
            continue;
        }
        if let Some(p) = by_pid.get(&pid)
            && !is_shell(&p.comm) {
                return true;
            }
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    false
}

/// Every pid in the subtrees rooted at `roots` (roots included), from
/// `(pid, parent)` edges. Cycle-safe.
pub fn descendants(
    edges: impl IntoIterator<Item = (u32, Option<u32>)>,
    roots: &[u32],
) -> HashSet<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for (pid, parent) in edges {
        if let Some(pp) = parent {
            children.entry(pp).or_default().push(pid);
        }
    }
    let mut seen = HashSet::new();
    let mut stack = roots.to_vec();
    while let Some(pid) = stack.pop() {
        if seen.insert(pid)
            && let Some(kids) = children.get(&pid)
        {
            stack.extend(kids.iter().copied());
        }
    }
    seen
}

/// `"34%"` — integer percent, clamped at zero so a transient negative delta
/// from sysinfo never prints a minus sign.
pub fn fmt_cpu(pct: f32) -> String {
    format!("{:.0}%", pct.max(0.0))
}

/// `"480 MB"` / `"2.1 GB"` — GB with one decimal at and above 1 GiB, whole MB
/// below it. Binary units, matching Activity Monitor's "memory" column.
pub fn fmt_mem(bytes: u64) -> String {
    const GIB: f64 = (1u64 << 30) as f64;
    const MIB: f64 = (1u64 << 20) as f64;
    let b = bytes as f64;
    if b >= GIB {
        format!("{:.1} GB", b / GIB)
    } else {
        format!("{} MB", (b / MIB) as u64)
    }
}

/// A fixed-length trail of recent totals for the sparkline. Oldest samples fall
/// off the front once it is full.
#[derive(Debug)]
pub struct History {
    samples: VecDeque<Usage>,
    cap: usize,
}

impl History {
    pub fn new(cap: usize) -> Self {
        History { samples: VecDeque::with_capacity(cap), cap: cap.max(1) }
    }

    pub fn push(&mut self, sample: Usage) {
        if self.samples.len() == self.cap {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
    }

    pub fn iter(&self) -> impl Iterator<Item = &Usage> {
        self.samples.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, parent: Option<u32>, cpu: f32, rss: u64) -> Proc {
        Proc { pid, parent, cpu_pct: cpu, rss_bytes: rss }
    }

    fn wt(key: &str, label: &str) -> Attributable {
        Attributable {
            label: label.to_string(),
            session_prefix: format!("harness_{key}_"),
        }
    }

    #[test]
    fn subtree_sums_a_multi_level_process_tree() {
        // pane 100 → 101 → 102, all under one worktree's session.
        let procs = [
            proc(1, None, 1.0, 100),     // app
            proc(100, Some(9), 5.0, 200),
            proc(101, Some(100), 3.0, 300),
            proc(102, Some(101), 2.0, 400),
        ];
        let snap = aggregate(
            &procs,
            1,
            &[("harness_wtA_7".to_string(), 100)],
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage { cpu_pct: 10.0, rss_bytes: 900 });
        assert_eq!(snap.base, Usage { cpu_pct: 1.0, rss_bytes: 100 });
        assert_eq!(snap.total, Usage { cpu_pct: 11.0, rss_bytes: 1000 });
    }

    #[test]
    fn total_equals_base_plus_every_worktree() {
        let procs = [
            proc(1, None, 2.0, 50),
            proc(200, Some(9), 4.0, 100), // wtA
            proc(300, Some(9), 6.0, 100), // wtB
        ];
        let snap = aggregate(
            &procs,
            1,
            &[
                ("harness_wtA_1".to_string(), 200),
                ("harness_wtB_1".to_string(), 300),
            ],
            &[wt("wtA", "A"), wt("wtB", "B")],
        );
        let sum: Usage = snap.per_worktree.iter().fold(snap.base, |mut acc, w| {
            acc.add(w.usage);
            acc
        });
        assert_eq!(snap.total, sum, "the accounting invariant must hold");
        assert_eq!(snap.total, Usage { cpu_pct: 12.0, rss_bytes: 250 });
    }

    #[test]
    fn a_pane_pid_is_counted_once_even_if_listed_twice() {
        // Two tabs of the same worktree can report overlapping subtrees; the
        // shared session (or a doubly-listed pane) must not double-count.
        let procs = [proc(1, None, 0.0, 0), proc(500, Some(9), 8.0, 800)];
        let snap = aggregate(
            &procs,
            1,
            &[
                ("harness_wtA_1".to_string(), 500),
                ("harness_wtA_2".to_string(), 500),
            ],
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage { cpu_pct: 8.0, rss_bytes: 800 });
    }

    #[test]
    fn multiple_sessions_of_one_worktree_merge_into_its_bucket() {
        let procs = [
            proc(1, None, 0.0, 0),
            proc(10, Some(9), 1.0, 10),
            proc(20, Some(9), 2.0, 20),
        ];
        let snap = aggregate(
            &procs,
            1,
            &[
                ("harness_wtA_1".to_string(), 10),
                ("harness_wtA_2".to_string(), 20),
            ],
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage { cpu_pct: 3.0, rss_bytes: 30 });
    }

    #[test]
    fn unrelated_sessions_are_excluded() {
        // A user's own tmux session, and a *different* worktree's — neither
        // belongs to wtA.
        let procs = [
            proc(1, None, 0.0, 0),
            proc(70, Some(9), 9.0, 900), // some other session's pane
        ];
        let snap = aggregate(
            &procs,
            1,
            &[
                ("my-own-session".to_string(), 70),
                ("harness_wtB_1".to_string(), 70),
            ],
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage::default(), "wtA claims nothing");
        assert_eq!(snap.total, Usage::default(), "unmatched sessions aren't in the total");
    }

    #[test]
    fn base_excludes_a_pane_that_sits_under_the_app() {
        // A non-tmux pane is a child of the app AND (hypothetically) listed as a
        // session pane — it must be counted once, in the worktree, not twice.
        let procs = [
            proc(1, None, 1.0, 100),      // app
            proc(2, Some(1), 4.0, 400),   // pane spawned directly under the app
        ];
        let snap = aggregate(
            &procs,
            1,
            &[("harness_wtA_1".to_string(), 2)],
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage { cpu_pct: 4.0, rss_bytes: 400 });
        assert_eq!(snap.base, Usage { cpu_pct: 1.0, rss_bytes: 100 }, "app only, pane not re-counted");
        assert_eq!(snap.total, Usage { cpu_pct: 5.0, rss_bytes: 500 });
    }

    #[test]
    fn a_cycle_in_the_parent_map_terminates() {
        // Pathological, but a bad parent chain must not hang the sampler.
        let procs = [proc(10, Some(11), 1.0, 1), proc(11, Some(10), 1.0, 1)];
        let snap = aggregate(&procs, 10, &[], &[]);
        assert_eq!(snap.base, Usage { cpu_pct: 2.0, rss_bytes: 2 });
    }

    #[test]
    fn an_exited_pane_pid_contributes_nothing() {
        let procs = [proc(1, None, 1.0, 100)];
        let snap = aggregate(
            &procs,
            1,
            &[("harness_wtA_1".to_string(), 999)], // 999 already gone
            &[wt("wtA", "A")],
        );
        assert_eq!(snap.per_worktree[0].usage, Usage::default());
    }

    #[test]
    fn descendants_walks_deep_and_survives_cycles() {
        let edges = [(2, Some(1)), (3, Some(2)), (4, Some(3)), (9, Some(8)), (1, Some(4))];
        let got = descendants(edges, &[2]);
        assert_eq!(got, HashSet::from([1, 2, 3, 4]));
        assert!(descendants(edges, &[]).is_empty());
    }

    #[test]
    fn cpu_formats_as_clamped_integer_percent() {
        assert_eq!(fmt_cpu(34.4), "34%");
        assert_eq!(fmt_cpu(0.0), "0%");
        assert_eq!(fmt_cpu(-2.0), "0%", "a negative delta reads as 0");
        assert_eq!(fmt_cpu(220.0), "220%", "a multi-core subtree may exceed 100");
    }

    #[test]
    fn memory_scales_mb_below_a_gig_and_gb_above() {
        assert_eq!(fmt_mem(0), "0 MB");
        assert_eq!(fmt_mem(512 * (1 << 20)), "512 MB");
        assert_eq!(fmt_mem(1 << 30), "1.0 GB");
        assert_eq!(fmt_mem((1 << 30) + (1 << 30) / 2), "1.5 GB");
    }

    #[test]
    fn sample_once_sees_this_very_process() {
        // Exercises the real sysinfo path (no mocks): our own process must show
        // up, resident, and tmux is skipped when unavailable.
        let mut sys = System::new();
        let raw = sample_once(&mut sys, false);
        let me = std::process::id();
        assert_eq!(raw.app_pid, me);
        let mine = raw
            .procs
            .iter()
            .find(|p| p.pid == me)
            .expect("the sampler must see our own pid");
        assert!(mine.rss_bytes > 0, "a running process holds some memory");
        assert!(raw.session_panes.is_empty(), "has_tmux=false skips the tmux read");
    }

    #[test]
    fn parse_ps_normalizes_comm_to_a_bare_basename() {
        // The exact shapes observed from `ps -axo pid=,ppid=,comm=`.
        let out = "97973 82836 -zsh\n99906 82836 sh\n99909 99906 /opt/test/bin/claude\n1 0 /sbin/launchd\n";
        let procs = parse_ps(out);
        assert_eq!(
            procs,
            vec![
                ProcInfo { pid: 97973, parent: Some(82836), comm: "zsh".into() },
                ProcInfo { pid: 99906, parent: Some(82836), comm: "sh".into() },
                ProcInfo { pid: 99909, parent: Some(99906), comm: "claude".into() },
                // ppid 0 → no parent.
                ProcInfo { pid: 1, parent: None, comm: "launchd".into() },
            ]
        );
    }

    #[test]
    fn a_pane_of_only_shells_has_no_agent() {
        // sample-project's stale case: the pane fell back to `-zsh`, agent long gone.
        let procs = parse_ps("97973 82836 -zsh\n98039 97973 -zsh\n");
        assert!(!agent_running_in(&procs, &[97973]), "only shells → no agent");
    }

    #[test]
    fn a_live_agent_under_the_pane_shell_is_detected() {
        // openOrchestra: pane runs `sh -lc`, with claude as its child.
        let procs = parse_ps("99906 82836 sh\n99909 99906 /opt/test/bin/claude\n");
        assert!(agent_running_in(&procs, &[99906]), "claude under the pane → alive");
    }

    #[test]
    fn an_agent_running_a_shell_tool_still_counts_as_alive() {
        // Guards against clearing a working agent mid-Bash-tool: the whole
        // subtree is searched, so claude is found even under a transient shell.
        let procs = parse_ps("100 82836 sh\n101 100 claude\n102 101 bash\n");
        assert!(agent_running_in(&procs, &[100]));
    }

    #[test]
    fn a_gone_session_has_no_agent() {
        // No live pane pids at all, or a root that no longer exists.
        let procs = parse_ps("1 0 launchd\n");
        assert!(!agent_running_in(&procs, &[]), "no roots → nothing alive");
        assert!(!agent_running_in(&procs, &[99999]), "vanished root → nothing alive");
    }

    #[test]
    fn history_drops_the_oldest_past_its_cap() {
        let mut h = History::new(3);
        for i in 1..=5u64 {
            h.push(Usage { cpu_pct: i as f32, rss_bytes: i });
        }
        assert_eq!(h.iter().count(), 3);
        let cpus: Vec<f32> = h.iter().map(|u| u.cpu_pct).collect();
        assert_eq!(cpus, vec![3.0, 4.0, 5.0], "oldest two fell off, order preserved");
    }
}
