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
        #[cfg(unix)]
        {
            let dir = std::env::var("TMUX_TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let uid = unsafe { libc::getuid() };
            let _ = std::fs::remove_file(format!("{dir}/tmux-{uid}/{}", self.socket));
        }
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

/// With the user's `detach-on-destroy off`, killing the watched session moves
/// the control client to another session instead of exiting it: the hub must
/// still report `stopped`, close, and not stay attached elsewhere.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn hub_reports_exit_with_detach_on_destroy_off() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    srv.new_session("keep", "cat");
    assert!(srv.ok(&["set-option", "-g", "detach-on-destroy", "off"]));
    let hubs = Hubs::new(srv.tmux());
    let hub = hubs.get("s").await.expect("hub");
    let mut rx = hub.subscribe();
    assert!(srv.ok(&["kill-session", "-t", "=s"]));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
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
    assert!(hub.is_closed());
    // The server-owned client must not linger on the unmarked session.
    assert!(
        poll_until(3, || srv
            .run(&["list-clients", "-t", "=keep", "-F", "#{client_name}"])
            .trim()
            .is_empty())
        .await,
        "control client still attached to 'keep'"
    );
    // A fresh hub is not handed out for the dead session.
    assert!(hubs.get("s").await.is_err());
}

/// A quick reply reaches the program as two reads: the text, then Enter.
/// One `text\r` write can look like a paste to an agent TUI.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn send_line_writes_text_then_enter_separately() {
    let srv = Server::new();
    // Raw mode: print each read() the program gets, as Python bytes reprs.
    srv.new_session(
        "s",
        "python3 -c 'import os,sys,tty; tty.setraw(0); print(\"READY\", end=\"\\r\\n\", flush=True); [print(\"R\" + repr(os.read(0, 1024)), end=\"\\r\\n\", flush=True) for _ in range(2)]; sys.stdin.read(1)'",
    );
    let hub = open(&srv, "s").await;
    let mut rx = hub.subscribe();
    assert!(
        poll_until(5, || srv
            .run(&["capture-pane", "-p", "-t", "=s:"])
            .contains("READY"))
        .await
    );
    hub.send_line(b"hello").await.expect("send_line");
    let out = wait_for(&mut rx, b"Rb'\\r'", 5).await;
    let out = String::from_utf8_lossy(&out);
    assert!(out.contains("Rb'hello'"), "{out:?}");
}

/// Quit: active fits are undone synchronously (argv tmux, bounded), even
/// while the guard is still held and the actor never runs again.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn restore_fits_blocking_undoes_active_fits() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let orig = srv.window_size("s");
    let hubs = Arc::new(Hubs::new(srv.tmux()));
    let hub = hubs.get("s").await.expect("hub");
    let guard = hub.fit(40, 12).await.expect("fit");
    assert_eq!(srv.window_size("s"), "40x12");
    let h2 = hubs.clone();
    let took = tokio::task::spawn_blocking(move || {
        let t = Instant::now();
        h2.restore_fits_blocking(Duration::from_secs(1));
        t.elapsed()
    })
    .await
    .unwrap();
    assert!(took <= Duration::from_millis(1100), "took {took:?}");
    // No polling: restored by the time the call returned.
    assert_eq!(srv.window_size("s"), orig);
    // Nothing left to restore a second time.
    let t = Instant::now();
    hubs.restore_fits_blocking(Duration::from_secs(1));
    assert!(t.elapsed() < Duration::from_millis(20));
    drop(guard);
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

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn cancelled_fit_is_released() {
    let srv = Server::new();
    srv.new_session("s", "cat");
    let orig = srv.window_size("s");
    let hub = open(&srv, "s").await;
    // Poll fit() once (queues the Fit command), then drop it mid-flight.
    {
        let fut = hub.fit(40, 20);
        tokio::pin!(fut);
        assert!(futures_poll_once(fut.as_mut()).await.is_none());
    }
    // Also cancel via a tiny timeout.
    let _ = tokio::time::timeout(Duration::from_micros(1), hub.fit(45, 22)).await;
    // Let the actor finish both in-flight fits and the resulting releases.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        poll_until(2, || srv.window_size("s") == orig).await,
        "cancelled fit left window at {}",
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
    // The refcount is back to zero: a fresh fit/drop cycle still restores.
    let g = hub.fit(40, 20).await.unwrap();
    assert_eq!(srv.window_size("s"), "40x20");
    drop(g);
    assert!(poll_until(2, || srv.window_size("s") == orig).await);
    hub.shutdown().await;
}

/// Poll a future exactly once; `Some` if it completed.
async fn futures_poll_once<F: std::future::Future>(
    mut fut: std::pin::Pin<&mut F>,
) -> Option<F::Output> {
    std::future::poll_fn(|cx| match fut.as_mut().poll(cx) {
        std::task::Poll::Ready(v) => std::task::Poll::Ready(Some(v)),
        std::task::Poll::Pending => std::task::Poll::Ready(None),
    })
    .await
}
