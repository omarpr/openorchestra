//! Integration tests for the status watcher, using real files in tempdirs and
//! the same atomic write strategy (temp file + rename) as the pi extension.

use std::fs;
use std::path::Path;
use std::time::Duration;

use status_watch::{AgentState, StatusEvent, StatusWatcher};

const RECV: Duration = Duration::from_secs(10);
/// Window in which we assert that *no* event arrives.
const QUIET: Duration = Duration::from_millis(400);

fn status_json(state: &str) -> String {
    format!(
        r#"{{"state": "{state}", "timestamp": "2026-07-19T12:00:00.000Z", "sessionFile": "/tmp/s.jsonl"}}"#
    )
}

/// Write the way the pi extension does: temp file in `.harness/` + rename.
fn write_atomic(root: &Path, content: &str) {
    let dir = root.join(".harness");
    fs::create_dir_all(&dir).unwrap();
    let tmp = dir.join(".status-test.tmp");
    fs::write(&tmp, content).unwrap();
    fs::rename(&tmp, dir.join("status.json")).unwrap();
}

fn expect_state(rx: &crossbeam_channel::Receiver<StatusEvent>, state: AgentState) -> StatusEvent {
    let event = rx.recv_timeout(RECV).expect("expected a status event");
    assert_eq!(event.status.state, state, "unexpected state in {event:?}");
    event
}

#[test]
fn missing_file_reports_unknown_on_watch() {
    let dir = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();

    let event = expect_state(&rx, AgentState::Unknown);
    assert_eq!(event.worktree_root, dir.path());
    assert_eq!(event.status.timestamp, None);
    assert_eq!(event.status.session_file, None);
}

#[test]
fn existing_file_reported_on_watch() {
    let dir = tempfile::tempdir().unwrap();
    write_atomic(dir.path(), &status_json("running"));

    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();

    let event = expect_state(&rx, AgentState::Running);
    assert_eq!(event.status.session_file.as_deref(), Some("/tmp/s.jsonl"));
    assert_eq!(
        event.status.timestamp.as_deref(),
        Some("2026-07-19T12:00:00.000Z")
    );
}

#[test]
fn atomic_rename_writes_deliver_updates() {
    let dir = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    expect_state(&rx, AgentState::Unknown); // initial

    write_atomic(dir.path(), &status_json("running"));
    expect_state(&rx, AgentState::Running);

    write_atomic(dir.path(), &status_json("waiting"));
    expect_state(&rx, AgentState::Waiting);

    write_atomic(dir.path(), &status_json("idle"));
    expect_state(&rx, AgentState::Idle);
}

#[test]
fn malformed_json_is_skipped_and_last_good_kept() {
    let dir = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    expect_state(&rx, AgentState::Unknown);

    write_atomic(dir.path(), &status_json("running"));
    expect_state(&rx, AgentState::Running);

    // Truncated / garbage writes must not produce events.
    write_atomic(dir.path(), r#"{"state": "run"#);
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "malformed JSON must not produce an event"
    );

    // Recovery: the next valid write comes through.
    write_atomic(dir.path(), &status_json("idle"));
    expect_state(&rx, AgentState::Idle);
}

#[test]
fn unknown_state_string_maps_to_unknown_variant() {
    let dir = tempfile::tempdir().unwrap();
    // Forward compatibility: a future state name parses as Unknown rather than
    // being dropped as malformed.
    write_atomic(dir.path(), &status_json("compacting"));

    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    let event = expect_state(&rx, AgentState::Unknown);
    assert_eq!(event.status.session_file.as_deref(), Some("/tmp/s.jsonl"));
}

#[test]
fn null_session_file_parses() {
    let dir = tempfile::tempdir().unwrap();
    write_atomic(
        dir.path(),
        r#"{"state": "idle", "timestamp": "2026-07-19T12:00:00Z", "sessionFile": null}"#,
    );
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    let event = expect_state(&rx, AgentState::Idle);
    assert_eq!(event.status.session_file, None);
}

#[test]
fn multiple_roots_are_keyed_correctly() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(a.path()).unwrap();
    watcher.watch(b.path()).unwrap();
    expect_state(&rx, AgentState::Unknown);
    expect_state(&rx, AgentState::Unknown);

    write_atomic(a.path(), &status_json("running"));
    let event = expect_state(&rx, AgentState::Running);
    assert_eq!(event.worktree_root, a.path());

    write_atomic(b.path(), &status_json("waiting"));
    let event = expect_state(&rx, AgentState::Waiting);
    assert_eq!(event.worktree_root, b.path());
}

#[test]
fn events_arrive_when_the_root_is_reached_through_a_symlink() {
    // Regression: the macOS FSEvents backend reports *canonicalized* paths, so
    // routing an event by `starts_with(<caller-supplied dir>)` silently matched
    // nothing whenever any path component was a symlink — and `/var` -> is
    // exactly that on macOS, which is where tempdirs live. Linux/inotify does
    // not canonicalize, so CI never saw it. Watch through a deliberate symlink
    // so the bug reproduces on every platform, not just macOS.
    let real = tempfile::tempdir().unwrap();
    let link_parent = tempfile::tempdir().unwrap();
    let link = link_parent.path().join("worktree-link");
    #[cfg(unix)]
    std::os::unix::fs::symlink(real.path(), &link).unwrap();

    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(&link).unwrap();
    expect_state(&rx, AgentState::Unknown);

    // Write through the *real* path; the watch was registered on the symlink.
    write_atomic(real.path(), &status_json("running"));
    let event = expect_state(&rx, AgentState::Running);
    // Events are reported under the path the caller registered, not the
    // resolved one — the registry key is an implementation detail.
    assert_eq!(event.worktree_root, link);

    // ...and unwatch must resolve identically, or it would leak the watch.
    watcher.unwatch(&link).unwrap();
    assert!(watcher.watched().is_empty());
}

#[test]
fn unwatch_stops_delivery() {
    let dir = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    expect_state(&rx, AgentState::Unknown);

    watcher.unwatch(dir.path()).unwrap();
    write_atomic(dir.path(), &status_json("running"));
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "no events after unwatch"
    );
    assert!(watcher.watched().is_empty());
}

#[test]
fn duplicate_content_is_not_redelivered() {
    let dir = tempfile::tempdir().unwrap();
    let (mut watcher, rx) = StatusWatcher::new().unwrap();
    watcher.watch(dir.path()).unwrap();
    expect_state(&rx, AgentState::Unknown);

    let payload = status_json("running");
    write_atomic(dir.path(), &payload);
    expect_state(&rx, AgentState::Running);

    // Identical content (same timestamp): the OS reports events, the watcher
    // dedupes them.
    write_atomic(dir.path(), &payload);
    assert!(
        rx.recv_timeout(QUIET).is_err(),
        "identical content must not be redelivered"
    );
}
