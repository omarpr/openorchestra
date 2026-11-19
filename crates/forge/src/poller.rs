//! Background poller: periodically asks a [`Forge`] about a set of branches
//! and delivers changed results over a channel. Interval + jitter + per-branch
//! exponential backoff on failure (R7).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::{CheckRun, CheckStatus, Forge, ForgeError, PrInfo, PrState};

/// Identifies one polled branch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BranchKey {
    pub repo_dir: PathBuf,
    pub branch: String,
}

/// Snapshot of forge data for a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub pr: Option<PrInfo>,
    pub checks: Vec<CheckRun>,
}

/// A poll outcome delivered to the UI. Errors are delivered once per failure
/// streak so the UI can gray out badges; identical consecutive successes are
/// suppressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForgeUpdate {
    pub key: BranchKey,
    pub result: Result<BranchStatus, ForgeError>,
}

#[derive(Debug, Clone)]
pub struct PollerConfig {
    /// Base interval between successful polls of the same branch.
    pub interval: Duration,
    /// Poll an open or draft PR sooner to catch new runs and reruns.
    pub open_pr_interval: Duration,
    /// Poll active checks sooner to catch completion.
    pub running_interval: Duration,
    /// Upper bound on the uniform random jitter added to each poll.
    pub jitter: Duration,
    /// First backoff step after a failure; doubles per consecutive failure.
    pub initial_backoff: Duration,
    /// Upper bound for the backoff.
    pub max_backoff: Duration,
}

impl Default for PollerConfig {
    fn default() -> Self {
        PollerConfig {
            interval: Duration::from_secs(45),
            open_pr_interval: Duration::from_secs(20),
            running_interval: Duration::from_secs(5),
            jitter: Duration::from_secs(10),
            initial_backoff: Duration::from_secs(15),
            max_backoff: Duration::from_secs(300),
        }
    }
}

struct BranchState {
    next_due: Instant,
    consecutive_failures: u32,
    last_delivered: Option<Result<BranchStatus, ForgeError>>,
}

/// Handle to the polling thread. Dropping (or calling [`Poller::stop`]) stops
/// the thread. Branches can be added/removed while running.
pub struct Poller {
    stop: Arc<AtomicBool>,
    branches: Arc<Mutex<Vec<BranchKey>>>,
    refresh_repos: Arc<Mutex<HashSet<PathBuf>>>,
    handle: Option<JoinHandle<()>>,
}

impl Poller {
    /// Spawn the poll loop. If `forge.available()` is `false`, the thread
    /// exits immediately and the receiver reports disconnected — callers hide
    /// badges (R7).
    pub fn spawn(
        forge: Arc<dyn Forge>,
        branches: Vec<BranchKey>,
        config: PollerConfig,
    ) -> (Poller, Receiver<ForgeUpdate>) {
        let (tx, rx) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let branches = Arc::new(Mutex::new(branches));
        let refresh_repos = Arc::new(Mutex::new(HashSet::new()));

        let thread_stop = Arc::clone(&stop);
        let thread_branches = Arc::clone(&branches);
        let thread_refresh_repos = Arc::clone(&refresh_repos);
        let handle = std::thread::Builder::new()
            .name("forge-poller".into())
            .spawn(move || {
                poll_loop(forge, thread_branches, thread_refresh_repos, config, thread_stop, tx)
            })
            .expect("failed to spawn forge-poller thread");

        (
            Poller {
                stop,
                branches,
                refresh_repos,
                handle: Some(handle),
            },
            rx,
        )
    }

    /// Add a branch to the polled set (no-op if already present). It becomes
    /// due immediately.
    pub fn add_branch(&self, key: BranchKey) {
        let mut branches = self.branches.lock().unwrap();
        if !branches.contains(&key) {
            branches.push(key);
        }
    }

    /// Remove a branch from the polled set.
    pub fn remove_branch(&self, key: &BranchKey) {
        self.branches.lock().unwrap().retain(|k| k != key);
    }

    /// Follow the branches currently shown by the workspace.
    pub fn set_branches(&self, branches: Vec<BranchKey>) {
        *self.branches.lock().unwrap() = branches;
    }

    /// Refresh a repository's branches and deliver a fresh snapshot for each.
    pub fn refresh_repo(&self, repo: &Path) {
        self.refresh_repos.lock().unwrap().insert(repo.to_owned());
    }

    /// Signal the thread to stop and wait for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Poller {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn poll_loop(
    forge: Arc<dyn Forge>,
    branches: Arc<Mutex<Vec<BranchKey>>>,
    refresh_repos: Arc<Mutex<HashSet<PathBuf>>>,
    config: PollerConfig,
    stop: Arc<AtomicBool>,
    tx: Sender<ForgeUpdate>,
) {
    if !forge.available() {
        return; // R7: gh missing — nothing to poll, badges stay hidden
    }

    let mut rng = Xorshift64::seeded();
    let mut states: HashMap<BranchKey, BranchState> = HashMap::new();
    // Responsive-stop tick; actual poll cadence is per-branch `next_due`.
    let tick = config.interval.min(Duration::from_millis(200)).max(Duration::from_millis(1));

    while !stop.load(Ordering::Relaxed) {
        let keys: Vec<BranchKey> = branches.lock().unwrap().clone();
        states.retain(|k, _| keys.contains(k));

        let now = Instant::now();
        let refresh = std::mem::take(&mut *refresh_repos.lock().unwrap());
        for key in keys {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let state = states.entry(key.clone()).or_insert_with(|| BranchState {
                next_due: now, // new branches are polled immediately
                consecutive_failures: 0,
                last_delivered: None,
            });
            if refresh.contains(&key.repo_dir) {
                state.next_due = now;
                state.last_delivered = None;
            }
            if state.next_due > now {
                continue;
            }

            let result = poll_branch(forge.as_ref(), &key);
            let (delay, jitter) = match &result {
                Ok(status) => {
                    state.consecutive_failures = 0;
                    success_schedule(&config, status)
                }
                Err(_) => {
                    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                    (backoff(&config, state.consecutive_failures), config.jitter)
                }
            };
            state.next_due = Instant::now() + delay + rng.jitter(jitter);

            // Deliver on change only; an ongoing failure streak is delivered
            // once (the first error), successes only when the data differs.
            if state.last_delivered.as_ref() != Some(&result) {
                state.last_delivered = Some(result.clone());
                if tx.send(ForgeUpdate { key, result }).is_err() {
                    return; // receiver gone — stop polling
                }
            }
        }

        std::thread::sleep(tick);
    }
}

fn poll_branch(forge: &dyn Forge, key: &BranchKey) -> Result<BranchStatus, ForgeError> {
    let pr = forge.pr_for_branch(&key.repo_dir, &key.branch)?;
    // No PR -> no checks call; saves half the gh invocations for fresh branches.
    let checks = if pr.is_some() {
        forge.checks_for_branch(&key.repo_dir, &key.branch)?
    } else {
        Vec::new()
    };
    Ok(BranchStatus { pr, checks })
}

fn success_schedule(config: &PollerConfig, status: &BranchStatus) -> (Duration, Duration) {
    let interval = if status.checks.iter().any(|check| check.status == CheckStatus::Running) {
        config.running_interval
    } else if status.pr.as_ref().is_some_and(|pr| matches!(pr.state, PrState::Open | PrState::Draft)) {
        config.open_pr_interval
    } else {
        return (config.interval, config.jitter);
    };
    let interval = interval.min(config.interval);
    (interval, config.jitter.min(interval / 5))
}

fn backoff(config: &PollerConfig, failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(16);
    let stepped = config
        .initial_backoff
        .saturating_mul(1u32 << shift);
    stepped.min(config.max_backoff)
}

/// Tiny PRNG for jitter — no need to pull in `rand` for this.
struct Xorshift64(u64);

impl Xorshift64 {
    fn seeded() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 ^ d.as_secs())
            .unwrap_or(0x9e3779b97f4a7c15);
        Xorshift64(nanos | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn jitter(&mut self, max: Duration) -> Duration {
        let max_nanos = max.as_nanos() as u64;
        if max_nanos == 0 {
            return Duration::ZERO;
        }
        Duration::from_nanos(self.next() % max_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_caps() {
        let config = PollerConfig {
            jitter: Duration::ZERO,
            initial_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(60),
            ..PollerConfig::default()
        };
        assert_eq!(backoff(&config, 1), Duration::from_secs(10));
        assert_eq!(backoff(&config, 2), Duration::from_secs(20));
        assert_eq!(backoff(&config, 3), Duration::from_secs(40));
        assert_eq!(backoff(&config, 4), Duration::from_secs(60));
        assert_eq!(backoff(&config, 30), Duration::from_secs(60)); // no overflow
    }

    #[test]
    fn checks_in_progress_poll_faster_until_they_finish() {
        let config = PollerConfig::default();
        let mut status = BranchStatus {
            pr: Some(PrInfo {
                number: 1,
                state: PrState::Open,
                title: String::new(),
                url: None,
            }),
            checks: vec![
                CheckRun { name: "build".into(), status: CheckStatus::Pass },
                CheckRun { name: "test".into(), status: CheckStatus::Running },
            ],
        };
        assert_eq!(success_schedule(&config, &status), (Duration::from_secs(5), Duration::from_secs(1)));
        status.checks[1].status = CheckStatus::Pass;
        assert_eq!(success_schedule(&config, &status), (Duration::from_secs(20), Duration::from_secs(4)));
        status.pr.as_mut().unwrap().state = PrState::Merged;
        assert_eq!(success_schedule(&config, &status), (config.interval, config.jitter));
    }

    #[test]
    fn jitter_is_bounded() {
        let mut rng = Xorshift64::seeded();
        for _ in 0..1000 {
            assert!(rng.jitter(Duration::from_secs(5)) < Duration::from_secs(5));
        }
        assert_eq!(rng.jitter(Duration::ZERO), Duration::ZERO);
    }
}
