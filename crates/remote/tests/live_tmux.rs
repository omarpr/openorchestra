//! Live tests against a real tmux (3.8). All `#[ignore]`: run with
//! `cargo test -p oo-remote --test live_tmux -- --ignored`.
//!
//! Every test runs on its own private socket (`-L oo-remote-test-<pid>-<n>`),
//! started with `-f /dev/null`, and kills that server on drop (also on panic).
//! The default tmux server is never touched.

use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oo_remote::tmux_bridge::{HubEvent, Hubs, SessionHub, Tmux};
use tokio::sync::broadcast;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// A private tmux server, killed on drop.
struct Server {
    socket: String,
}

impl Server {
    fn new() -> Server {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let socket = format!("oo-remote-test-{}-{}", std::process::id(), n);
        assert_ne!(socket, "default");
        Server { socket }
    }

    fn tmux(&self) -> Tmux {
        Tmux {
            socket: Some(self.socket.clone()),
        }
    }

    /// Run a tmux command on the private server (config disabled), return stdout.
    fn run(&self, args: &[&str]) -> String {
        let out = StdCommand::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .output()
            .expect("run tmux");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn ok(&self, args: &[&str]) -> bool {
        StdCommand::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .output()
            .expect("run tmux")
            .status
            .success()
    }

    fn new_session(&self, name: &str, cmd: &str) {
        assert!(
            self.ok(&[
                "new-session",
                "-d",
                "-s",
                name,
                "-x",
                "100",
                "-y",
                "30",
                cmd
            ]),
            "new-session {name}"
        );
    }

    fn window_size(&self, session: &str) -> String {
        self.run(&[
            "display-message",
            "-p",
            "-t",
            &format!("={session}:"),
            "#{window_width}x#{window_height}",
        ])
        .trim()
        .to_string()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = StdCommand::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("kill-server")
            .output();
        let dir = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let uid = unsafe { libc::getuid() };
        let _ = std::fs::remove_file(format!("{dir}/tmux-{uid}/{}", self.socket));
    }
}

/// Collect `Data` until `needle` appears in the accumulated output or timeout.
async fn wait_for(rx: &mut broadcast::Receiver<HubEvent>, needle: &[u8], secs: u64) -> Vec<u8> {
    let mut acc = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if acc.windows(needle.len()).any(|w| w == needle) {
            return acc;
        }
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(HubEvent::Data(b))) => acc.extend_from_slice(&b),
            Ok(Ok(HubEvent::Closed(r))) => panic!("closed ({r}) before {needle:?}; got {acc:?}"),
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

async fn poll_until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

async fn open(srv: &Server, session: &str) -> Arc<SessionHub> {
    SessionHub::open(srv.tmux(), session.to_string())
        .await
        .expect("open hub")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn stream_and_type_roundtrip() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    hub.send_input(b"hello\r").await.unwrap();
    wait_for(&mut rx, b"hello", 2).await;
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn utf8_input_roundtrip() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    hub.send_input("é🙂\r".as_bytes()).await.unwrap();
    wait_for(&mut rx, "é🙂".as_bytes(), 2).await;
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn prefix_keys_stay_in_pane() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    srv.new_session("other", "cat");
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    hub.send_input(b"\x02s\x02(\x02:kill-server\r")
        .await
        .unwrap();
    // `cat` writes the raw bytes back (the tty echo renders them as ^B).
    wait_for(&mut rx, b"\x02s\x02(\x02:kill-server", 2).await;
    assert!(srv.ok(&["has-session", "-t", "=s"]), "s died");
    assert!(srv.ok(&["has-session", "-t", "=other"]), "other died");
    let clients = srv.run(&["list-clients", "-F", "#{client_session}"]);
    for line in clients.lines() {
        assert_eq!(line, "s", "a client switched sessions: {clients:?}");
    }
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn ignore_size_keeps_window() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let before = srv.window_size("s");
    assert_eq!(before, "100x30");
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    hub.send_input(b"x\r").await.unwrap();
    wait_for(&mut rx, b"x", 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(srv.window_size("s"), before);
    hub.shutdown().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(srv.window_size("s"), before);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn fit_and_restore() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let orig = srv.window_size("s");
    let hub = open(&srv, "s").await;

    let g = hub.fit(40, 20).await.unwrap();
    assert_eq!(srv.window_size("s"), "40x20");
    drop(g);
    assert!(
        poll_until(2, || srv.window_size("s") == orig).await,
        "not restored: {}",
        srv.window_size("s")
    );
    assert!(
        poll_until(2, || srv
            .run(&["show-options", "-w", "-t", "=s:"])
            .trim()
            .is_empty())
        .await,
        "window-size still set locally"
    );

    let g1 = hub.fit(40, 20).await.unwrap();
    let g2 = hub.fit(40, 20).await.unwrap();
    drop(g1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(srv.window_size("s"), "40x20");
    drop(g2);
    assert!(poll_until(2, || srv.window_size("s") == orig).await);
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn shutdown_with_guard_restores() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let orig = srv.window_size("s");
    let hub = open(&srv, "s").await;
    let g = hub.fit(50, 15).await.unwrap();
    assert_eq!(srv.window_size("s"), "50x15");
    hub.shutdown().await;
    assert!(poll_until(2, || srv.window_size("s") == orig).await);
    drop(g); // no-op after shutdown
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(srv.window_size("s"), orig);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn hub_reports_exit() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    srv.new_session("keep", "cat");
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    assert!(srv.ok(&["kill-session", "-t", "=s"]));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        match tokio::time::timeout_at(deadline, rx.recv()).await {
            Ok(Ok(HubEvent::Closed(r))) => {
                assert_eq!(r, "stopped");
                break;
            }
            Ok(Ok(HubEvent::Data(_))) => {}
            other => panic!("no Closed: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn init_frame_matches_pane() {
    let srv = Server::new();
    srv.new_session("s", "printf 'line1\\nline2'; exec cat");
    assert!(
        poll_until(2, || srv
            .run(&["capture-pane", "-p", "-t", "=s:"])
            .contains("line2"))
        .await
    );
    let hub = open(&srv, "s").await;
    let _rx = hub.subscribe();
    let f = hub.init_frame().await.unwrap();
    let text = String::from_utf8_lossy(&f.bytes);
    assert!(text.contains("line1"), "{text:?}");
    assert!(text.contains("line2"), "{text:?}");
    assert!(f.bytes.starts_with(b"\x1b[2J\x1b[H"));
    let size = srv.run(&[
        "display-message",
        "-p",
        "-t",
        "=s:",
        "#{pane_width} #{pane_height}",
    ]);
    assert_eq!(size.trim(), format!("{} {}", f.cols, f.rows));
    // cursor sits after "line2" on row 2 -> ESC[2;6H
    assert!(f.bytes.ends_with(b"\x1b[2;6H"), "{text:?}");
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn open_requires_exact_session() {
    let srv = Server::new();
    srv.new_session("sx", "cat");
    assert!(SessionHub::open(srv.tmux(), "s".into()).await.is_err());
    assert!(SessionHub::open(srv.tmux(), "sx:".into()).await.is_err());
    assert!(SessionHub::open(srv.tmux(), "nope".into()).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn other_panes_not_forwarded() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    // second window in the same session, also running cat
    assert!(srv.ok(&["new-window", "-d", "-t", "=s:", "cat"]));
    let other_pane = srv
        .run(&[
            "list-panes",
            "-s",
            "-t",
            "=s",
            "-F",
            "#{pane_id} #{window_active}",
        ])
        .lines()
        .find(|l| l.ends_with(" 0"))
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .to_string();
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    assert!(srv.ok(&["send-keys", "-t", &other_pane, "SECRET", "Enter"]));
    hub.send_input(b"mine\r").await.unwrap();
    let got = wait_for(&mut rx, b"mine", 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut more = got;
    while let Ok(HubEvent::Data(b)) = rx.try_recv() {
        more.extend_from_slice(&b);
    }
    assert!(!more.windows(6).any(|w| w == b"SECRET"));
    hub.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn hubs_share_and_reopen() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let hubs = Hubs::new(srv.tmux());
    let a = hubs.get("s").await.unwrap();
    let b = hubs.get("s").await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    drop(a);
    drop(b);
    let c = hubs.get("s").await.unwrap();
    let mut rx = c.subscribe();
    c.send_input(b"again\r").await.unwrap();
    wait_for(&mut rx, b"again", 2).await;
    hubs.close("s").await;
    let d = hubs.get("s").await.unwrap();
    assert!(!Arc::ptr_eq(&c, &d));
    d.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn live_sessions_lists_names() {
    let srv = Server::new();
    assert!(oo_remote::tmux_bridge::live_sessions(&srv.tmux())
        .await
        .is_empty());
    srv.new_session("s", "cat");
    srv.new_session("other", "cat");
    let names = oo_remote::tmux_bridge::live_sessions(&srv.tmux()).await;
    assert!(names.contains("s") && names.contains("other") && names.len() == 2);
}
