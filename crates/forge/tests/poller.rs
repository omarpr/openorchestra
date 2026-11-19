//! Poller behavior tests with a scripted in-memory Forge.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forge::{
    BranchKey, CheckRun, CheckStatus, Forge, ForgeError, Poller, PollerConfig, PrInfo, PrState,
};

const RECV: Duration = Duration::from_secs(10);
const QUIET: Duration = Duration::from_millis(300);

fn fast_config() -> PollerConfig {
    PollerConfig {
        interval: Duration::from_millis(20),
        open_pr_interval: Duration::from_millis(20),
        running_interval: Duration::from_millis(20),
        jitter: Duration::from_millis(5),
        initial_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(80),
    }
}

fn key(branch: &str) -> BranchKey {
    BranchKey {
        repo_dir: PathBuf::from("/repos/demo"),
        branch: branch.to_string(),
    }
}

fn pr(number: u64) -> PrInfo {
    PrInfo {
        number,
        state: PrState::Open,
        title: format!("PR #{number}"),
        url: None,
    }
}

/// Scripted forge: pops pre-programmed results per call; repeats the last one
/// when the script runs dry.
struct ScriptedForge {
    available: bool,
    pr_script: Mutex<Vec<Result<Option<PrInfo>, ForgeError>>>,
    checks: Result<Vec<CheckRun>, ForgeError>,
    calls: AtomicUsize,
}

impl ScriptedForge {
    fn new(pr_script: Vec<Result<Option<PrInfo>, ForgeError>>) -> Self {
        ScriptedForge {
            available: true,
            pr_script: Mutex::new(pr_script),
            checks: Ok(vec![CheckRun {
                name: "build".into(),
                status: CheckStatus::Pass,
            }]),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Forge for ScriptedForge {
    fn create_pr(
        &self,
        _: &std::path::Path,
        _: &forge::CreatePrRequest,
    ) -> Result<forge::PrInfo, ForgeError> {
        // The poller must never mutate the forge; if it ever calls this, that
        // is the bug worth failing loudly on.
        panic!("the poller must not create pull requests");
    }

    fn available(&self) -> bool {
        self.available
    }

    fn pr_for_branch(&self, _r: &Path, _b: &str) -> Result<Option<PrInfo>, ForgeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut script = self.pr_script.lock().unwrap();
        if script.len() > 1 {
            Ok(script.remove(0)?)
        } else {
            script[0].clone()
        }
    }

    fn checks_for_branch(&self, _r: &Path, _b: &str) -> Result<Vec<CheckRun>, ForgeError> {
        self.checks.clone()
    }
}

#[test]
fn delivers_initial_status_and_deduplicates() {
    let forge = Arc::new(ScriptedForge::new(vec![Ok(Some(pr(7)))]));
    let (poller, rx) = Poller::spawn(forge, vec![key("feat/a")], fast_config());

    let update = rx.recv_timeout(RECV).unwrap();
    assert_eq!(update.key, key("feat/a"));
    let status = update.result.unwrap();
    assert_eq!(status.pr.as_ref().unwrap().number, 7);
    assert_eq!(status.checks.len(), 1);

    // Same data on every subsequent poll -> no more deliveries.
    assert!(rx.recv_timeout(QUIET).is_err());
    poller.stop();
}

#[test]
fn refreshing_a_repository_delivers_its_checks_before_the_next_poll() {
    let forge = Arc::new(ScriptedForge::new(vec![Ok(Some(pr(7)))]));
    let branch = key("feat/a");
    let other = BranchKey { repo_dir: PathBuf::from("/repos/other"), ..branch.clone() };
    let config = PollerConfig {
        interval: Duration::from_secs(60),
        open_pr_interval: Duration::from_secs(60),
        ..fast_config()
    };
    let (poller, rx) = Poller::spawn(forge, vec![branch.clone(), other], config);
    let initial = rx.recv_timeout(RECV).unwrap();
    assert_eq!(initial.key, branch);
    rx.recv_timeout(RECV).unwrap();

    poller.refresh_repo(&branch.repo_dir);
    assert_eq!(rx.recv_timeout(RECV).unwrap(), initial);
    assert!(rx.recv_timeout(QUIET).is_err());
    poller.stop();
}

#[test]
fn delivers_changes() {
    let forge = Arc::new(ScriptedForge::new(vec![
        Ok(None),
        Ok(Some(pr(7))),
    ]));
    let (poller, rx) = Poller::spawn(forge, vec![key("feat/a")], fast_config());

    let first = rx.recv_timeout(RECV).unwrap().result.unwrap();
    assert_eq!(first.pr, None);
    assert_eq!(first.checks, vec![]); // no PR -> checks not fetched

    let second = rx.recv_timeout(RECV).unwrap().result.unwrap();
    assert_eq!(second.pr.as_ref().unwrap().number, 7);
    poller.stop();
}

#[test]
fn failure_is_delivered_once_then_recovery() {
    let forge = Arc::new(ScriptedForge::new(vec![
        Err(ForgeError::CommandFailed {
            message: "rate limited".into(),
        }),
        Err(ForgeError::CommandFailed {
            message: "rate limited".into(),
        }),
        Ok(Some(pr(9))),
    ]));
    let (poller, rx) = Poller::spawn(forge, vec![key("feat/a")], fast_config());

    let first = rx.recv_timeout(RECV).unwrap();
    assert!(first.result.is_err());

    // The identical repeated error is suppressed; next delivery is the recovery.
    let next = rx.recv_timeout(RECV).unwrap();
    let status = next.result.expect("second delivery should be the recovery");
    assert_eq!(status.pr.as_ref().unwrap().number, 9);
    poller.stop();
}

#[test]
fn unavailable_forge_polls_nothing() {
    let mut forge = ScriptedForge::new(vec![Ok(None)]);
    forge.available = false;
    let forge = Arc::new(forge);
    let (poller, rx) = Poller::spawn(Arc::clone(&forge) as Arc<dyn Forge>, vec![key("feat/a")], fast_config());

    // Thread exits immediately: channel disconnects without any updates.
    match rx.recv_timeout(RECV) {
        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {}
        other => panic!("expected disconnect, got {other:?}"),
    }
    assert_eq!(forge.calls.load(Ordering::SeqCst), 0);
    poller.stop();
}

#[test]
fn add_and_remove_branches_at_runtime() {
    let forge = Arc::new(ScriptedForge::new(vec![Ok(Some(pr(1)))]));
    let (poller, rx) = Poller::spawn(Arc::clone(&forge) as Arc<dyn Forge>, vec![], fast_config());

    assert!(rx.recv_timeout(QUIET).is_err(), "no branches, no updates");

    poller.add_branch(key("feat/b"));
    let update = rx.recv_timeout(RECV).unwrap();
    assert_eq!(update.key, key("feat/b"));

    poller.remove_branch(&key("feat/b"));
    poller.stop();
}
