//! Background poller: periodically asks a [`Forge`] about a set of branches
//! and delivers changed results over a channel. Interval + jitter + per-branch
//! exponential backoff on failure (R7).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::{CheckRun, Forge, ForgeError, PrInfo};

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
    /// Uniform random jitter added to every scheduled poll, `0..jitter`.
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

        let thread_stop = Arc::clone(&stop);
        let thread_branches = Arc::clone(&branches);
        let handle = std::thread::Builder::new()
            .name("forge-poller".into())
            .spawn(move || poll_loop(forge, thread_branches, config, thread_stop, tx))
            .expect("failed to spawn forge-poller thread");

        (
            Poller {
                stop,
                branches,
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
        for key in keys {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            let state = states.entry(key.clone()).or_insert_with(|| BranchState {
                next_due: now, // new branches are polled immediately
                consecutive_failures: 0,
                last_delivered: None,
            });
            if state.next_due > now {
                continue;
            }

            let result = poll_branch(forge.as_ref(), &key);
            let delay = match &result {
                Ok(_) => {
                    state.consecutive_failures = 0;
                    config.interval
                }
                Err(_) => {
                    state.consecutive_failures = state.consecutive_failures.saturating_add(1);
                    backoff(&config, state.consecutive_failures)
                }
            };
            state.next_due = Instant::now() + delay + rng.jitter(config.jitter);

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
            interval: Duration::from_secs(45),
            jitter: Duration::ZERO,
            initial_backoff: Duration::from_secs(10),
            max_backoff: Duration::from_secs(60),
        };
        assert_eq!(backoff(&config, 1), Duration::from_secs(10));
        assert_eq!(backoff(&config, 2), Duration::from_secs(20));
        assert_eq!(backoff(&config, 3), Duration::from_secs(40));
        assert_eq!(backoff(&config, 4), Duration::from_secs(60));
        assert_eq!(backoff(&config, 30), Duration::from_secs(60)); // no overflow
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
