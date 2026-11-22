//! The phone remote's tmux bridge (`oo_remote::tmux_bridge`) against this
//! binary: session discovery, control-mode attach, typing, fitting and exit.
//!
//! Every test runs on its own private label (`-L oo-bridge-test-<pid>-<n>`)
//! and kills its sessions on drop (also on panic), so the app's own sessions
//! are never touched.
#![cfg(windows)]

use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use oo_remote::tmux_bridge::{self, HubEvent, Hubs, SessionHub, Tmux};
use tokio::sync::broadcast;

const BIN: &str = env!("CARGO_BIN_EXE_oo-session");

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A private label, its sessions killed on drop.
struct Label(String);

impl Label {
    fn new() -> Label {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        Label(format!("oo-bridge-test-{}-{n}", std::process::id()))
    }

    fn tmux(&self) -> Tmux {
        Tmux {
            socket: Some(self.0.clone()),
            program: Some(BIN.into()),
        }
    }

    fn run(&self, args: &[&str]) -> (bool, String) {
        let out = StdCommand::new(BIN)
            .arg("-L")
            .arg(&self.0)
            .args(args)
            .output()
            .expect("run oo-session");
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    /// A 100x30 `cmd.exe` session.
    fn new_session(&self, name: &str) {
        let (ok, _) = self.run(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            "100",
            "-y",
            "30",
            "cmd.exe",
        ]);
        assert!(ok, "new-session {name}");
    }

    fn size(&self, session: &str) -> String {
        let fmt = "#{window_width}x#{window_height}";
        self.run(&["display-message", "-p", "-t", &format!("={session}:"), fmt])
            .1
            .trim()
            .to_string()
    }
}

impl Drop for Label {
    fn drop(&mut self) {
        let _ = self.run(&["kill-server"]);
    }
}

/// Collect `Data` until `needle` appears in the output, or panic.
async fn wait_for(rx: &mut broadcast::Receiver<HubEvent>, needle: &[u8], secs: u64) -> Vec<u8> {
    let mut acc = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if acc.windows(needle.len()).any(|w| w == needle) {
            return acc;
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(HubEvent::Data(b))) => acc.extend_from_slice(&b),
            Ok(Ok(HubEvent::Closed(r))) => panic!("closed ({r}) before {needle:?}"),
            Ok(Err(broadcast::error::RecvError::Lagged(_))) => {}
            Ok(Err(e)) => panic!("recv: {e}"),
            Err(_) => panic!(
                "timeout waiting for {:?}; got {:?}",
                String::from_utf8_lossy(needle),
                String::from_utf8_lossy(&acc)
            ),
        }
    }
}

async fn open(label: &Label, session: &str) -> Arc<SessionHub> {
    SessionHub::open(label.tmux(), session.to_string())
        .await
        .expect("open hub")
}

#[tokio::test(flavor = "multi_thread")]
async fn discovery_lists_only_this_labels_sessions() {
    let label = Label::new();
    let names = tmux_bridge::try_live_sessions(&label.tmux()).await.unwrap();
    assert!(names.is_empty(), "{names:?}");
    label.new_session("bridge_a");
    label.new_session("bridge b");
    let names = tmux_bridge::live_sessions(&label.tmux()).await;
    assert_eq!(names, ["bridge_a", "bridge b"].map(String::from).into());
    let other = Label::new();
    assert!(tmux_bridge::live_sessions(&other.tmux()).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn attach_streams_output_and_types_input() {
    let label = Label::new();
    label.new_session("s");
    label.run(&["send-keys", "-t", "=s", "echo BEFORE_ATTACH", "Enter"]);

    let hub = open(&label, "s").await;
    let mut rx = hub.subscribe();
    // Attaching in control mode leaves the session's size alone.
    assert_eq!(label.size("s"), "100x30");

    // The screen so far, through capture-pane and the pane's geometry.
    let frame = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = hub.init_frame().await.expect("init frame");
            if String::from_utf8_lossy(&frame.bytes).contains("BEFORE_ATTACH") {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("BEFORE_ATTACH in the initial frame");
    assert_eq!((frame.cols, frame.rows), (100, 30));

    // Live output, from keys the phone types (send-keys -H).
    hub.send_input(b"echo PHONE_").await.unwrap();
    hub.send_input("é_OK\r".as_bytes()).await.unwrap();
    wait_for(&mut rx, "PHONE_é_OK".as_bytes(), 10).await;

    // A quick reply: text, then Enter on its own.
    hub.send_line(b"echo QUICK_REPLY").await.unwrap();
    wait_for(&mut rx, b"QUICK_REPLY\r\n", 10).await;
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn fit_resizes_and_the_last_guard_restores() {
    let label = Label::new();
    label.new_session("fit");
    let hub = open(&label, "fit").await;
    let first = hub.fit(60, 20).await.unwrap();
    assert_eq!(label.size("fit"), "60x20");
    let second = hub.fit(50, 15).await.unwrap();
    assert_eq!(label.size("fit"), "50x15");
    drop(first);
    drop(second);
    let restored = tokio::time::timeout(Duration::from_secs(5), async {
        while label.size("fit") != "100x30" {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(
        restored.is_ok(),
        "size after release: {}",
        label.size("fit")
    );
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_closes_when_the_session_ends() {
    let label = Label::new();
    label.new_session("gone");
    let hub = open(&label, "gone").await;
    let mut rx = hub.subscribe();
    assert!(label.run(&["kill-session", "-t", "=gone"]).0);
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(HubEvent::Closed(reason)) => return reason,
                Ok(HubEvent::Data(_)) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("recv: {e}"),
            }
        }
    })
    .await
    .expect("Closed after kill-session");
    assert_eq!(closed, "stopped");
    assert!(hub.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn open_needs_an_existing_exact_session() {
    let label = Label::new();
    label.new_session("exact_name");
    for name in ["exact", "missing", "exact_name:0"] {
        let err = SessionHub::open(label.tmux(), name.to_string()).await.err();
        assert!(err.is_some(), "{name} opened");
    }
    let hubs = Hubs::new(label.tmux());
    let a = hubs.get("exact_name").await.unwrap();
    let b = hubs.get("exact_name").await.unwrap();
    assert!(Arc::ptr_eq(&a, &b), "one hub per session");
    hubs.close("exact_name").await;
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while !a.is_closed() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(closed.is_ok(), "the control client did not exit");
    // The session itself lives on.
    assert!(label.run(&["has-session", "-t", "=exact_name"]).0);
}
