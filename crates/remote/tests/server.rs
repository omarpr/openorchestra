//! HTTPS server / API tests. Each test gets a temp state dir, binds
//! 127.0.0.1 on port 0, and pins the cert SANs to `localhost` + 127.0.0.1.
//! Tests that need tmux are `#[ignore]` and use a private `-L` socket; the
//! non-live tests point at an unused private socket so the user's default
//! tmux server is never touched.
//!
//! Live: `cargo test -p oo-remote --test server -- --ignored`.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use futures_util::{SinkExt, StreamExt};
use oo_remote::tmux_bridge::Tmux;
use oo_remote::{
    auth, snapshot_channel, AgentInfo, RemoteServer, ServerConfig, Snapshot, StartError, Store,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, ServerName};
use tempfile::TempDir;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

const CSP: &str = "default-src 'self'; connect-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'";

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn unique_socket() -> String {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let s = format!("oo-remote-srvtest-{}-{}", std::process::id(), n);
    assert_ne!(s, "default");
    s
}

/// A private tmux server, killed on drop.
struct TmuxServer {
    socket: String,
}

impl TmuxServer {
    fn new() -> TmuxServer {
        TmuxServer {
            socket: unique_socket(),
        }
    }

    fn tmux(&self) -> Tmux {
        Tmux {
            socket: Some(self.socket.clone()),
        }
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        StdCommand::new("tmux")
            .arg("-L")
            .arg(&self.socket)
            .arg("-f")
            .arg("/dev/null")
            .args(args)
            .output()
            .expect("run tmux")
    }

    fn new_session(&self, name: &str, cmd: &str) {
        let out = self.cmd(&[
            "new-session",
            "-d",
            "-s",
            name,
            "-x",
            "100",
            "-y",
            "30",
            cmd,
        ]);
        assert!(out.status.success(), "new-session {name}");
    }

    fn capture(&self, session: &str) -> String {
        let out = self.cmd(&["capture-pane", "-p", "-t", &format!("={session}:")]);
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

impl Drop for TmuxServer {
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

fn free_port() -> u16 {
    StdListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn config(dir: &Path, port: u16, setup_port: u16, tmux: Tmux) -> ServerConfig {
    ServerConfig {
        state_dir: dir.to_path_buf(),
        port,
        setup_port,
        tmux,
        bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
        hosts_override: Some((
            vec!["localhost".to_string()],
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        )),
    }
}

fn ca_root(dir: &Path) -> CertificateDer<'static> {
    CertificateDer::from_pem_file(dir.join("remote/ca.crt")).expect("ca.crt")
}

fn tls_config(dir: &Path) -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_root(dir)).unwrap();
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth()
}

struct Harness {
    _tmp: TempDir,
    dir: PathBuf,
    server: Option<RemoteServer>,
    snap: watch::Sender<Snapshot>,
    port: u16,
    setup_port: u16,
    client: reqwest::Client,
}

impl Harness {
    fn start(tmux: Tmux) -> Harness {
        Harness::start_with(tmux, Duration::from_secs(60))
    }

    fn start_with(tmux: Tmux, cert_check: Duration) -> Harness {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("state");
        let setup_port = free_port();
        let (snap, rx) = snapshot_channel();
        let server =
            RemoteServer::start_with_cert_check(config(&dir, 0, setup_port, tmux), rx, cert_check)
                .unwrap_or_else(|e| panic!("start: {e:?}"));
        let port = server.port();
        assert_ne!(port, 0);
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls_config(&dir))
            .resolve("localhost", SocketAddr::from(([127, 0, 0, 1], port)))
            .build()
            .unwrap();
        Harness {
            _tmp: tmp,
            dir,
            server: Some(server),
            snap,
            port,
            setup_port,
            client,
        }
    }

    fn unused_tmux() -> Tmux {
        Tmux {
            socket: Some(unique_socket()),
        }
    }

    fn server(&self) -> &RemoteServer {
        self.server.as_ref().unwrap()
    }

    fn store(&self) -> Store {
        Store::new(&self.dir)
    }

    fn url(&self, path: &str) -> String {
        format!("https://localhost:{}{path}", self.port)
    }

    /// A paired device's token (created through the store, like the app).
    fn device(&self, read_only: bool) -> String {
        let store = self.store();
        let code = auth::issue_code(&store, SystemTime::now()).unwrap();
        let (dev, token) =
            auth::redeem_code(&store, &code, "test phone", "localhost", SystemTime::now()).unwrap();
        if read_only {
            auth::set_read_only(&store, &dev.id, true).unwrap();
        }
        token
    }

    fn set_agents(&self, agents: Vec<AgentInfo>) {
        self.snap.send_replace(Snapshot { agents });
    }

    async fn get(&self, path: &str, token: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(self.url(path));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        req.send().await.expect("request")
    }

    async fn send_text(&self, id: &str, token: &str, text: &str) -> reqwest::StatusCode {
        self.client
            .post(self.url(&format!("/api/agents/{id}/send")))
            .bearer_auth(token)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await
            .expect("send")
            .status()
    }

    async fn agents(&self, token: &str) -> Vec<serde_json::Value> {
        let r = self.get("/api/agents", Some(token)).await;
        assert_eq!(r.status(), 200);
        r.json().await.unwrap()
    }

    async fn ws(
        &self,
        id: &str,
        origin: &str,
    ) -> Result<
        tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>,
        WsError,
    > {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", self.port))
            .await
            .unwrap();
        let mut cfg = tls_config(&self.dir);
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls = tokio_rustls::TlsConnector::from(Arc::new(cfg))
            .connect(ServerName::try_from("localhost").unwrap(), tcp)
            .await
            .unwrap();
        let mut req = format!("wss://localhost:{}/api/term/{id}", self.port)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert("Origin", origin.parse().unwrap());
        tokio_tungstenite::client_async(req, tls)
            .await
            .map(|(ws, _)| ws)
    }

    fn origin(&self) -> String {
        format!("https://localhost:{}", self.port)
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(s) = self.server.take() {
            s.stop();
        }
    }
}

fn agent(id: &str, session: &str, title: &str) -> AgentInfo {
    AgentInfo {
        id: id.to_string(),
        title: title.to_string(),
        agent_kind: Some("claude".into()),
        project: "proj".into(),
        worktree: "main".into(),
        status: "running".into(),
        status_since: None,
        tool: None,
        branch: None,
        has_conversation: false,
        session: session.to_string(),
        transcript: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_pwa_shell_with_csp() {
    let h = Harness::start(Harness::unused_tmux());
    let r = h.get("/", None).await;
    // Without `web/remote/dist` the shell is the "run `make web`" 503 page.
    let expected = if oo_remote::assets::WEB_CLIENT_BUILT {
        200
    } else {
        503
    };
    assert_eq!(r.status(), expected);
    assert_eq!(
        r.headers()
            .get("content-security-policy")
            .expect("csp header"),
        CSP
    );
    assert!(r
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/html"));
    let body = r.text().await.unwrap();
    if !oo_remote::assets::WEB_CLIENT_BUILT {
        assert!(body.contains("make web"), "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn api_requires_token() {
    let h = Harness::start(Harness::unused_tmux());
    assert_eq!(h.get("/api/agents", None).await.status(), 401);
    assert_eq!(h.get("/api/agents", Some("bogus")).await.status(), 401);
    assert_eq!(h.get("/api/me", Some("bogus")).await.status(), 401);
    assert_eq!(h.get("/api/events", None).await.status(), 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_then_list_only_marked() {
    let h = Harness::start(Harness::unused_tmux());
    let store = h.store();
    let a = store.mark("k", 1).unwrap();
    let b = store.mark("k", 2).unwrap();
    h.set_agents(vec![
        agent(&a, "s_a", "A"),
        agent(&b, "s_b", "B"),
        agent("r_cccccccccccccccc", "s_c", "C"),
    ]);

    let code = auth::issue_code(&store, SystemTime::now()).unwrap();
    let r = h
        .client
        .post(h.url("/api/pair"))
        .json(&serde_json::json!({ "code": code.to_lowercase(), "device_name": "My Phone" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let body: serde_json::Value = r.json().await.unwrap();
    let token = body["token"].as_str().expect("token").to_string();
    let dev = &store.load().devices[0];
    assert_eq!(dev.name, "My Phone");
    assert_eq!(dev.origin_host, "localhost");

    let me: serde_json::Value = h.get("/api/me", Some(&token)).await.json().await.unwrap();
    assert_eq!(
        me,
        serde_json::json!({ "name": "My Phone", "read_only": false })
    );

    let ids: Vec<String> = h
        .agents(&token)
        .await
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, vec![a.clone(), b.clone()]);

    store.unmark("k", Some(2)).unwrap();
    let list = h.agents(&token).await;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], a.as_str());
    assert_eq!(list[0]["title"], "A");
    assert!(list[0].get("session").is_none(), "session must not leak");
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_rate_limited() {
    let h = Harness::start(Harness::unused_tmux());
    for i in 0..5 {
        let r = h
            .client
            .post(h.url("/api/pair"))
            .json(&serde_json::json!({ "code": "2222-2222", "device_name": "x" }))
            .send()
            .await
            .unwrap();
        assert_ne!(r.status(), 429, "attempt {}", i + 1);
        assert!(r.status() == 401 || r.status() == 423, "{}", r.status());
    }
    let r = h
        .client
        .post(h.url("/api/pair"))
        .json(&serde_json::json!({ "code": "2222-2222", "device_name": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_closes_sse() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let mut r = h.get("/api/events", Some(&token)).await;
    assert_eq!(r.status(), 200);
    assert!(r
        .headers()
        .get("content-type")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/event-stream"));
    let first = tokio::time::timeout(Duration::from_secs(2), r.chunk())
        .await
        .expect("first event")
        .unwrap()
        .expect("chunk");
    let first = String::from_utf8_lossy(&first);
    assert!(first.contains("event: agents"), "{first}");
    assert!(first.contains("data: []"), "{first}");

    // Revoke from a separate Store (as the other build or the UI would).
    let other = Store::new(&h.dir);
    let id = other.load().devices[0].id.clone();
    auth::revoke(&other, &id).unwrap();

    let ended = tokio::time::timeout(Duration::from_secs(2), async {
        while let Ok(Some(_)) = r.chunk().await {}
    })
    .await;
    assert!(ended.is_ok(), "SSE stream still open 2s after revoke");
}

#[tokio::test(flavor = "multi_thread")]
async fn sse_pushes_agent_changes() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    let mut r = h.get("/api/events", Some(&token)).await;
    let mut acc = String::new();
    let first = r.chunk().await.unwrap().unwrap();
    acc.push_str(&String::from_utf8_lossy(&first));
    assert!(acc.contains("data: []"), "{acc}");
    h.set_agents(vec![agent(&a, "s_a", "A")]);
    let got = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let c = r.chunk().await.unwrap().unwrap();
            acc.push_str(&String::from_utf8_lossy(&c));
            if acc.contains(&a) {
                return acc;
            }
        }
    })
    .await
    .expect("agents event");
    assert!(got.contains("\"status\":\"stopped\""), "{got}");
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_id_404_and_unmarked_id_404() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let store = h.store();
    let a = store.mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_a", "A")]);
    assert_eq!(h.send_text("r_nope", &token, "hi").await, 404);
    store.unmark("k", Some(1)).unwrap();
    assert_eq!(h.send_text(&a, &token, "hi").await, 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn send_to_stopped_is_409_and_oversize_413() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "no_such_session", "A")]);
    assert_eq!(h.send_text(&a, &token, "hi").await, 409);
    let big = "x".repeat(16 * 1024 + 1);
    assert_eq!(h.send_text(&a, &token, &big).await, 413);
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_rejects_bad_origin() {
    let h = Harness::start(Harness::unused_tmux());
    let origin = format!("https://evil.local:{}", h.port);
    match h.ws("r_x", &origin).await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status(), 403),
        Err(e) => panic!("unexpected error {e}"),
        Ok(_) => panic!("upgrade accepted with a foreign Origin"),
    }
    // Wrong port is foreign too.
    let origin = format!("https://localhost:{}", h.port.wrapping_add(1));
    assert!(matches!(h.ws("r_x", &origin).await, Err(WsError::Http(r)) if r.status() == 403));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_requires_auth_frame() {
    let h = Harness::start(Harness::unused_tmux());
    let mut ws = h.ws("r_x", &h.origin()).await.expect("upgrade");
    let start = Instant::now();
    let msg = tokio::time::timeout(Duration::from_secs(8), ws.next())
        .await
        .expect("closed within 8s");
    match msg {
        Some(Ok(Message::Close(Some(frame)))) => {
            assert_eq!(frame.code, CloseCode::from(4401));
        }
        other => panic!("expected close 4401, got {other:?}"),
    }
    assert!(start.elapsed() >= Duration::from_secs(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_bad_token_closes_4401() {
    let h = Harness::start(Harness::unused_tmux());
    let mut ws = h.ws("r_x", &h.origin()).await.expect("upgrade");
    ws.send(Message::text(r#"{"type":"auth","token":"bogus"}"#))
        .await
        .unwrap();
    match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        Ok(Some(Ok(Message::Close(Some(frame))))) => {
            assert_eq!(frame.code, CloseCode::from(4401))
        }
        other => panic!("expected close 4401, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ws_unmarked_id_reports_unmarked() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let mut ws = h.ws("r_unknown", &h.origin()).await.expect("upgrade");
    ws.send(Message::text(
        serde_json::json!({ "type": "auth", "token": token }).to_string(),
    ))
    .await
    .unwrap();
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
    assert_eq!(
        v,
        serde_json::json!({ "type": "closed", "reason": "unmarked" })
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_cannot_send() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(true);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_a", "A")]);
    assert_eq!(h.send_text(&a, &token, "hi").await, 403);
    let me: serde_json::Value = h.get("/api/me", Some(&token)).await.json().await.unwrap();
    assert_eq!(me["read_only"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn cert_reload_on_ip_change() {
    let h = Harness::start(Harness::unused_tmux());
    let new_ip: IpAddr = "10.123.45.67".parse().unwrap();
    h.server().test_set_hosts(
        vec!["localhost".to_string()],
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST), new_ip],
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let ips = presented_ips(&h).await;
        if ips.contains(&new_ip) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "cert not reloaded within 1s: {ips:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(h
        .server()
        .status()
        .urls
        .contains(&format!("https://10.123.45.67:{}/", h.port)));
}

/// IP SANs of the certificate the server presents on a fresh handshake.
async fn presented_ips(h: &Harness) -> Vec<IpAddr> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", h.port))
        .await
        .unwrap();
    let tls = tokio_rustls::TlsConnector::from(Arc::new(tls_config(&h.dir)))
        .connect(ServerName::try_from("localhost").unwrap(), tcp)
        .await
        .unwrap();
    let der = tls.get_ref().1.peer_certificates().unwrap()[0].clone();
    let (_, cert) = x509_parser::parse_x509_certificate(&der).unwrap();
    let san = cert.subject_alternative_name().unwrap().unwrap();
    san.value
        .general_names
        .iter()
        .filter_map(|n| match n {
            x509_parser::extensions::GeneralName::IPAddress(b) if b.len() == 4 => {
                Some(IpAddr::from(<[u8; 4]>::try_from(*b).unwrap()))
            }
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_port_only_when_open() {
    let h = Harness::start(Harness::unused_tmux());
    let plain = reqwest::Client::new();
    let setup = |p: &str| format!("http://127.0.0.1:{}{p}", h.setup_port);
    let before = plain.get(setup("/setup")).send().await;
    assert!(
        before.as_ref().is_err_and(|e| e.is_connect()),
        "setup port answered while closed: {before:?}"
    );

    h.server().set_setup_open(true);
    let r = plain.get(setup("/setup")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let page = r.text().await.unwrap();
    assert!(page.contains(&h.server().status().ca_fingerprint), "{page}");
    // iOS only installs profiles from Safari, and "Profile Downloaded" is
    // short-lived — the page must say both.
    assert!(page.contains("Safari"), "{page}");
    assert!(page.contains("VPN &amp; Device Management"), "{page}");
    assert!(
        !page.contains("id=\"use-safari\""),
        "banner shown to a non-iOS client"
    );

    // Chrome on iPhone gets a banner telling it to reopen this page in Safari.
    let chrome_ios = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_1 like Mac OS X) \
        AppleWebKit/605.1.15 (KHTML, like Gecko) CriOS/131.0.6778.73 Mobile/15E148 Safari/604.1";
    let r = plain
        .get(setup("/setup"))
        .header("user-agent", chrome_ios)
        .send()
        .await
        .unwrap();
    let page = r.text().await.unwrap();
    assert!(page.contains("id=\"use-safari\""), "{page}");
    assert!(
        page.contains(&setup("/setup")),
        "banner should show this page's URL: {page}"
    );

    let r = plain.get(setup("/ca.crt")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "application/x-x509-ca-cert");
    let r = plain.get(setup("/ca.mobileconfig")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        r.headers()["content-type"],
        "application/x-apple-aspen-config"
    );
    for p in ["/api/agents", "/", "/api/pair", "/index.html"] {
        let r = plain.get(setup(p)).send().await.unwrap();
        assert_eq!(r.status(), 404, "{p}");
    }

    h.server().set_setup_open(false);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if plain.get(setup("/setup")).send().await.is_err() {
            break;
        }
        assert!(Instant::now() < deadline, "setup port still open");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn setup_port_bind_failure_is_reported() {
    let h = Harness::start(Harness::unused_tmux());
    assert_eq!(h.server().status().setup_error, None);
    // Another app holds the setup port.
    let blocker = std::net::TcpListener::bind(("127.0.0.1", h.setup_port)).unwrap();
    h.server().set_setup_open(true);
    let err = h
        .server()
        .status()
        .setup_error
        .expect("bind failure reported");
    assert!(err.contains(&h.setup_port.to_string()), "{err}");
    assert!(err.contains("another app"), "{err}");

    // Closing the panel clears it; reopening once the port is free works.
    h.server().set_setup_open(false);
    assert_eq!(h.server().status().setup_error, None);
    drop(blocker);
    h.server().set_setup_open(true);
    assert_eq!(h.server().status().setup_error, None);
    let r = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{}/setup", h.setup_port))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    h.server().set_setup_open(false);
}

#[tokio::test(flavor = "multi_thread")]
async fn port_in_use() {
    let tmp = TempDir::new().unwrap();
    let (_tx, rx) = snapshot_channel();
    let a = RemoteServer::start(
        config(
            &tmp.path().join("a"),
            0,
            free_port(),
            Harness::unused_tmux(),
        ),
        rx.clone(),
    )
    .expect("first server");
    // Port 0 lets the OS pick a port that is held by `a` from the start; a
    // free_port() probe is released and can be taken by a parallel test.
    let port = a.port();
    let b = RemoteServer::start(
        config(
            &tmp.path().join("b"),
            port,
            free_port(),
            Harness::unused_tmux(),
        ),
        rx,
    );
    match b {
        Err(StartError::PortInUse(p)) => assert_eq!(p, port),
        Err(e) => panic!("expected PortInUse, got {e:?}"),
        Ok(_) => panic!("second server started on a used port"),
    }
    a.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_sends_disabled_to_sse() {
    let mut h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let mut r = h.get("/api/events", Some(&token)).await;
    r.chunk().await.unwrap().unwrap();
    let srv = h.server.take().unwrap();
    std::thread::spawn(move || srv.stop());
    let tail = tokio::time::timeout(Duration::from_secs(3), async {
        let mut acc = String::new();
        while let Ok(Some(c)) = r.chunk().await {
            acc.push_str(&String::from_utf8_lossy(&c));
        }
        acc
    })
    .await
    .expect("SSE still open after stop");
    assert!(tail.contains("event: closed"), "{tail}");
    assert!(tail.contains(r#"{"reason":"disabled"}"#), "{tail}");
}

#[tokio::test(flavor = "multi_thread")]
async fn revoke_sends_closed_revoked_on_sse() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    let mut r = h.get("/api/events", Some(&token)).await;
    r.chunk().await.unwrap().unwrap();
    let other = Store::new(&h.dir);
    let id = other.load().devices[0].id.clone();
    auth::revoke(&other, &id).unwrap();
    let tail = tokio::time::timeout(Duration::from_secs(2), async {
        let mut acc = String::new();
        while let Ok(Some(c)) = r.chunk().await {
            acc.push_str(&String::from_utf8_lossy(&c));
        }
        acc
    })
    .await
    .expect("SSE still open after revoke");
    assert!(tail.contains(r#"{"reason":"revoked"}"#), "{tail}");
}

// ---- live tmux (private socket) ----

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn send_flattens_newlines() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "cat");
    let h = Harness::start(tm.tmux());
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    assert_eq!(h.send_text(&a, &token, "a\nb").await, 204);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let screen = tm.capture("s_live");
        // Echo of the typed line, then cat's copy after Enter.
        if screen.lines().filter(|l| l.trim_end() == "a b").count() >= 2 {
            break;
        }
        assert!(Instant::now() < deadline, "pane: {screen:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn next_msg<S>(ws: &mut tokio_tungstenite::WebSocketStream<S>) -> Message
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(3), ws.next())
        .await
        .expect("message within 3s")
        .expect("stream open")
        .expect("ws ok")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn each_ws_gets_init_frame() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "cat");
    let h = Harness::start(tm.tmux());
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    for round in 0..2 {
        let mut ws = h.ws(&a, &h.origin()).await.expect("upgrade");
        ws.send(Message::text(
            serde_json::json!({ "type": "auth", "token": token }).to_string(),
        ))
        .await
        .unwrap();
        let init = next_msg(&mut ws).await;
        let v: serde_json::Value = serde_json::from_str(init.to_text().unwrap()).unwrap();
        assert_eq!(v["type"], "init", "round {round}: {v}");
        assert_eq!(v["cols"], 100);
        assert_eq!(v["rows"], 30);
        assert_eq!(v["read_only"], false);
        let bin = next_msg(&mut ws).await;
        match bin {
            Message::Binary(b) => assert!(b.starts_with(b"\x1b[0m"), "{b:?}"),
            other => panic!("expected binary init bytes, got {other:?}"),
        }
        // Live data after input.
        ws.send(Message::text(
            serde_json::json!({ "type": "input", "data": format!("ping{round}\r") }).to_string(),
        ))
        .await
        .unwrap();
        let needle = format!("ping{round}");
        let mut acc = Vec::new();
        while !String::from_utf8_lossy(&acc).contains(&needle) {
            match next_msg(&mut ws).await {
                Message::Binary(b) => acc.extend_from_slice(&b),
                other => panic!("unexpected {other:?}"),
            }
        }
        ws.close(None).await.ok();
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn dead_session_listed_as_stopped() {
    let tm = TmuxServer::new();
    tm.new_session("s_alive", "cat");
    let h = Harness::start(tm.tmux());
    let token = h.device(false);
    let store = h.store();
    let a = store.mark("k", 1).unwrap();
    let b = store.mark("k", 2).unwrap();
    h.set_agents(vec![
        agent(&a, "s_alive", "alive"),
        agent(&b, "s_gone", "gone"),
    ]);
    let list = h.agents(&token).await;
    let status = |id: &str| {
        list.iter()
            .find(|x| x["id"] == id)
            .map(|x| x["status"].as_str().unwrap().to_string())
    };
    assert_eq!(status(&a).as_deref(), Some("running"));
    assert_eq!(status(&b).as_deref(), Some("stopped"));
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// Open a terminal socket on `id`, authenticate, and consume init + repaint.
async fn open_term(h: &Harness, id: &str, token: &str) -> Ws {
    let mut ws = h.ws(id, &h.origin()).await.expect("upgrade");
    ws.send(Message::text(
        serde_json::json!({ "type": "auth", "token": token }).to_string(),
    ))
    .await
    .unwrap();
    let init = next_msg(&mut ws).await;
    let v: serde_json::Value = serde_json::from_str(init.to_text().unwrap()).unwrap();
    assert_eq!(v["type"], "init", "{v}");
    assert!(matches!(next_msg(&mut ws).await, Message::Binary(_)));
    ws
}

/// Skip binary output until the `closed` frame; return its reason.
async fn closed_reason(ws: &mut Ws) -> String {
    loop {
        match next_msg(ws).await {
            Message::Binary(_) => continue,
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["type"], "closed", "{v}");
                return v["reason"].as_str().unwrap().to_string();
            }
            other => panic!("expected closed frame, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn ws_reauthorizes_on_file_change_and_stop() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "cat");
    let mut h = Harness::start(tm.tmux());
    let token = h.device(false);
    let ro_token = h.device(true);
    let store = h.store();
    let a = store.mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    let other = Store::new(&h.dir);

    // Unmark from another Store closes with `unmarked`.
    let mut ws = open_term(&h, &a, &token).await;
    other.unmark("k", Some(1)).unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(2), closed_reason(&mut ws))
        .await
        .expect("unmarked within 2s");
    assert_eq!(reason, "unmarked");

    // Re-mark (new id), then revoke closes with `revoked`.
    let a = store.mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    let mut ws = open_term(&h, &a, &token).await;
    let dev_id = other
        .load()
        .devices
        .iter()
        .find(|d| !d.read_only)
        .unwrap()
        .id
        .clone();
    auth::revoke(&other, &dev_id).unwrap();
    let reason = tokio::time::timeout(Duration::from_secs(2), closed_reason(&mut ws))
        .await
        .expect("revoked within 2s");
    assert_eq!(reason, "revoked");

    // Read-only input never reaches the pane.
    let mut ro = open_term(&h, &a, &ro_token).await;
    ro.send(Message::text(
        serde_json::json!({ "type": "input", "data": "SHOULD_NOT_APPEAR\r" }).to_string(),
    ))
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!tm.capture("s_live").contains("SHOULD_NOT_APPEAR"));

    // stop() sends `disabled`.
    let srv = h.server.take().unwrap();
    std::thread::spawn(move || srv.stop());
    let reason = tokio::time::timeout(Duration::from_secs(4), closed_reason(&mut ro))
        .await
        .expect("disabled on stop");
    assert_eq!(reason, "disabled");
}

#[tokio::test(flavor = "multi_thread")]
async fn cert_check_runs_despite_file_churn() {
    let h = Harness::start_with(Harness::unused_tmux(), Duration::from_millis(300));
    // remote.json changes every 100ms, faster than the check period.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let churn = {
        let (dir, stop) = (h.dir.clone(), stop.clone());
        std::thread::spawn(move || {
            let store = Store::new(&dir);
            let mut n = 0u16;
            while !stop.load(Ordering::SeqCst) {
                n = n.wrapping_add(1);
                store.update(|f| f.port = 7000 + (n % 100)).unwrap();
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    tokio::time::sleep(Duration::from_millis(250)).await;
    let new_ip: IpAddr = "10.77.0.1".parse().unwrap();
    // No wake-up: only the periodic check can pick this up.
    h.server().test_set_hosts_quiet(
        vec!["localhost".to_string()],
        vec![IpAddr::V4(Ipv4Addr::LOCALHOST), new_ip],
    );
    let deadline = Instant::now() + Duration::from_millis(1500);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        seen = presented_ips(&h).await;
        if seen.contains(&new_ip) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    stop.store(true, Ordering::SeqCst);
    churn.join().unwrap();
    assert!(
        seen.contains(&new_ip),
        "cert never rechecked under churn: {seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn ws_input_not_starved_by_output() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "sh");
    let h = Harness::start(tm.tmux());
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    let mut ws = open_term(&h, &a, &token).await;
    let input =
        |d: &str| Message::text(serde_json::json!({ "type": "input", "data": d }).to_string());
    ws.send(input("while :; do cat /usr/share/dict/words; done\r"))
        .await
        .unwrap();
    // Wait until the flood is on.
    let mut acc = Vec::new();
    while acc.len() < 2_000_000 {
        if let Message::Binary(b) = next_msg(&mut ws).await {
            acc.extend_from_slice(&b);
        }
    }
    let t0 = Instant::now();
    ws.send(input("\x03")).await.unwrap();
    ws.send(input("echo DONE_$((40+2))\r")).await.unwrap();
    let mut tail: Vec<u8> = Vec::new();
    while !String::from_utf8_lossy(&tail).contains("DONE_42") {
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "Ctrl-C did not stop the output loop within 3s"
        );
        match tokio::time::timeout(Duration::from_secs(3), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                tail.extend_from_slice(&b);
                if tail.len() > 1 << 16 {
                    tail.drain(..tail.len() - 4096);
                }
            }
            Ok(Some(Ok(_))) => {}
            other => panic!("socket ended: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn send_after_unmark_during_open_is_404() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "cat");
    let h = Harness::start(tm.tmux());
    let token = h.device(false);
    let store = h.store();
    let a = store.mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);
    // Widen the window between opening the session and typing.
    h.server()
        .test_delay_send_after_open(Duration::from_millis(800));
    let send = h.send_text(&a, &token, "LEAKED_TEXT");
    let unmark = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        Store::new(&h.dir).unmark("k", Some(1)).unwrap();
    };
    let (status, ()) = tokio::join!(send, unmark);
    assert_eq!(status, 404);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!tm.capture("s_live").contains("LEAKED_TEXT"));
}

fn window_size(tm: &TmuxServer, session: &str) -> String {
    let out = tm.cmd(&[
        "display-message",
        "-p",
        "-t",
        &format!("={session}:"),
        "#{window_width}x#{window_height}",
    ]);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

async fn wait_size(tm: &TmuxServer, session: &str, want: &str, secs: u64) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if window_size(tm, session) == want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

/// A phone that stops answering (left Wi-Fi with "Fit to phone" on) must not
/// hold the desktop window at phone size: the idle limit drops its socket and
/// with it the fit. A client that keeps reading (and so answers pings) stays.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn silent_client_times_out_and_releases_fit() {
    let tm = TmuxServer::new();
    tm.new_session("s_live", "cat");
    let h = Harness::start(tm.tmux());
    h.server()
        .test_set_ws_keepalive(Duration::from_millis(100), Duration::from_millis(600));
    let token = h.device(false);
    let a = h.store().mark("k", 1).unwrap();
    h.set_agents(vec![agent(&a, "s_live", "A")]);

    let mut live = open_term(&h, &a, &token).await;
    let mut silent = open_term(&h, &a, &token).await;
    silent
        .send(Message::text(
            serde_json::json!({ "type": "fit", "on": true, "cols": 40, "rows": 12 }).to_string(),
        ))
        .await
        .unwrap();
    assert!(
        wait_size(&tm, "s_live", "40x12", 3).await,
        "fit not applied"
    );

    // `silent` is never read again (no pongs); `live` keeps reading.
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(100), live.next()).await {
            Err(_) => {}
            Ok(Some(Ok(Message::Ping(_) | Message::Binary(_)))) => {}
            other => panic!("live client closed: {other:?}"),
        }
    }
    assert_eq!(window_size(&tm, "s_live"), "100x30", "fit not released");
    // The reading client is still served.
    live.send(Message::text(
        serde_json::json!({ "type": "input", "data": "still here\r" }).to_string(),
    ))
    .await
    .unwrap();
    let mut acc = Vec::new();
    while !String::from_utf8_lossy(&acc).contains("still here") {
        match next_msg(&mut live).await {
            Message::Binary(b) => acc.extend_from_slice(&b),
            Message::Ping(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    drop(silent);
}

/// A transient bad read of remote.json (unparseable mid-write, permission
/// glitch) must not look like a mass revoke: requests keep using the last
/// good copy, and the file is not rewritten.
#[tokio::test(flavor = "multi_thread")]
async fn unparseable_remote_json_keeps_last_good_copy() {
    let h = Harness::start(Harness::unused_tmux());
    let token = h.device(false);
    assert_eq!(h.get("/api/me", Some(&token)).await.status(), 200);
    // Let the watcher pick up the paired device as the last good copy.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let path = h.store().path().to_path_buf();
    std::fs::write(&path, b"{ \"devices\": [ trunc").unwrap();
    // Past the file loop's poll, so the watcher has seen the bad file too.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(h.get("/api/me", Some(&token)).await.status(), 200);
    assert_eq!(h.get("/api/agents", Some(&token)).await.status(), 200);
    assert_eq!(std::fs::read(&path).unwrap(), b"{ \"devices\": [ trunc");
    // An unknown token is still refused.
    assert_eq!(h.get("/api/me", Some("nope")).await.status(), 401);
}

// ---- conversation view ----

/// Grouped so `cargo test --test server conversation` runs them all.
mod conversation {
    use super::*;
    use oo_remote::{parse_entries, TranscriptKind};

    const FIXTURE: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/claude-transcript.jsonl"
    );

    /// Reads `event:`/`data:` pairs off an SSE response (comments skipped).
    struct SseReader {
        r: reqwest::Response,
        buf: String,
    }

    impl SseReader {
        /// The next event, or `None` when the stream ended. Panics after 3 s.
        async fn next(&mut self) -> Option<(String, String)> {
            tokio::time::timeout(Duration::from_secs(3), self.next_inner())
                .await
                .expect("no SSE event within 3s")
        }

        async fn next_inner(&mut self) -> Option<(String, String)> {
            loop {
                while let Some(i) = self.buf.find("\n\n") {
                    let block: String = self.buf.drain(..i + 2).collect();
                    let (mut event, mut data) = (None, Vec::new());
                    for line in block.lines() {
                        if let Some(v) = line.strip_prefix("event:") {
                            event = Some(v.trim().to_string());
                        } else if let Some(v) = line.strip_prefix("data:") {
                            data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
                        }
                    }
                    if event.is_some() || !data.is_empty() {
                        return Some((event.unwrap_or_else(|| "message".into()), data.join("\n")));
                    }
                }
                match self.r.chunk().await {
                    Ok(Some(c)) => self.buf.push_str(&String::from_utf8_lossy(&c)),
                    _ => return None,
                }
            }
        }

        /// The next `entries` event as (replace, entries).
        async fn entries(&mut self) -> (bool, Vec<serde_json::Value>) {
            let (ev, data) = self.next().await.expect("stream ended");
            assert_eq!(ev, "entries", "{data}");
            let v: serde_json::Value = serde_json::from_str(&data).unwrap();
            (
                v["replace"].as_bool().expect("replace"),
                v["entries"].as_array().expect("entries").clone(),
            )
        }
    }

    fn conv_agent(id: &str, kind: &str, path: &Path) -> AgentInfo {
        let mut a = agent(id, "s_conv", "C");
        a.agent_kind = Some(kind.into());
        a.transcript = Some(path.to_path_buf());
        a
    }

    fn copy_fixture(dir: &Path) -> PathBuf {
        let p = dir.join("session.jsonl");
        std::fs::copy(FIXTURE, &p).unwrap();
        p
    }

    fn append(p: &Path, line: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(p).unwrap();
        writeln!(f, "{line}").unwrap();
    }

    fn ids(entries: &[serde_json::Value]) -> Vec<String> {
        entries
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect()
    }

    async fn open(h: &Harness, id: &str, token: &str) -> SseReader {
        let r = h
            .get(&format!("/api/agents/{id}/conversation"), Some(token))
            .await;
        assert_eq!(r.status(), 200);
        assert!(r
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"));
        SseReader {
            r,
            buf: String::new(),
        }
    }

    /// A marked Claude Code agent on a copy of the fixture.
    fn setup(h: &Harness, tmp: &TempDir) -> (String, PathBuf) {
        let path = copy_fixture(tmp.path());
        let a = h.store().mark("k", 1).unwrap();
        h.set_agents(vec![conv_agent(&a, "claude-code", &path)]);
        (a, path)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_streams_entries() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, path) = setup(&h, &tmp);
        let list = h.agents(&token).await;
        assert_eq!(list[0]["has_conversation"], true);

        let mut sse = open(&h, &a, &token).await;
        let (replace, entries) = sse.entries().await;
        assert!(replace);
        let text = std::fs::read_to_string(&path).unwrap();
        let want: Vec<String> = parse_entries(TranscriptKind::ClaudeCode, &text, None)
            .iter()
            .map(|e| e.id.clone())
            .collect();
        assert!(!want.is_empty());
        assert_eq!(ids(&entries), want);

        append(
            &path,
            r#"{"type":"assistant","uuid":"a5","parentUuid":"a4","isSidechain":false,"message":{"role":"assistant","content":[{"type":"text","text":"More."}]}}"#,
        );
        let (replace, entries) = tokio::time::timeout(Duration::from_secs(2), sse.entries())
            .await
            .expect("update within 2s");
        assert!(!replace);
        assert!(entries.iter().any(|e| e["text"] == "More."), "{entries:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_requires_mark_and_support() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let path = copy_fixture(tmp.path());
        let a = h.store().mark("k", 1).unwrap();
        h.set_agents(vec![
            conv_agent(&a, "codex", &path),
            conv_agent("r_unmarked000000", "claude-code", &path),
        ]);
        assert_eq!(
            h.get(&format!("/api/agents/{a}/conversation"), None)
                .await
                .status(),
            401
        );
        for id in ["r_unmarked000000", "r_nope", a.as_str()] {
            let r = h
                .get(&format!("/api/agents/{id}/conversation"), Some(&token))
                .await;
            assert_eq!(r.status(), 404, "{id}");
            let r = h
                .get(
                    &format!("/api/agents/{id}/conversation/tool/toolu_1"),
                    Some(&token),
                )
                .await;
            assert_eq!(r.status(), 404, "{id}");
        }
        let list = h.agents(&token).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0]["has_conversation"], false);
        assert!(list[0].get("branch").is_some(), "branch is sent");

        // Supported kind but no transcript known yet.
        let mut no_path = conv_agent(&a, "claude-code", &path);
        no_path.transcript = None;
        h.set_agents(vec![no_path]);
        let r = h
            .get(&format!("/api/agents/{a}/conversation"), Some(&token))
            .await;
        assert_eq!(r.status(), 404);
        assert_eq!(h.agents(&token).await[0]["has_conversation"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn transcript_path_never_sent() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, path) = setup(&h, &tmp);
        let path_str = path.to_string_lossy().to_string();
        let dir_name = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let clean = |what: &str, body: &str| {
            assert!(!body.contains(&path_str), "{what} leaks the path: {body}");
            assert!(!body.contains(&dir_name), "{what} leaks the dir: {body}");
            assert!(!body.contains("transcript"), "{what}: {body}");
        };

        let body = h
            .get("/api/agents", Some(&token))
            .await
            .text()
            .await
            .unwrap();
        clean("/api/agents", &body);

        let mut ev = h.get("/api/events", Some(&token)).await;
        let first = ev.chunk().await.unwrap().unwrap();
        clean("/api/events", &String::from_utf8_lossy(&first));

        let mut sse = open(&h, &a, &token).await;
        let (_, data) = sse.next().await.unwrap();
        clean("conversation", &data);

        for tool in ["toolu_1", "toolu_2", "nope"] {
            let body = h
                .get(
                    &format!("/api/agents/{a}/conversation/tool/{tool}"),
                    Some(&token),
                )
                .await
                .text()
                .await
                .unwrap();
            clean("tool", &body);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn new_transcript_path_replaces_stream() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, _) = setup(&h, &tmp);
        let mut sse = open(&h, &a, &token).await;
        let (replace, entries) = sse.entries().await;
        assert!(replace && !entries.is_empty());

        // A status-only snapshot change sends nothing.
        let mut same = h.snap.borrow().agents[0].clone();
        same.status = "waiting".into();
        h.set_agents(vec![same]);
        // Let the stream see it on its own (watch values coalesce).
        tokio::time::sleep(Duration::from_millis(300)).await;

        let fresh = tmp.path().join("new.jsonl");
        std::fs::write(
            &fresh,
            r#"{"type":"user","uuid":"n1","parentUuid":null,"message":{"role":"user","content":"brand new"}}"#
                .to_string()
                + "\n",
        )
        .unwrap();
        h.set_agents(vec![conv_agent(&a, "claude-code", &fresh)]);
        let (replace, entries) = sse.entries().await;
        assert!(replace);
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0]["text"], "brand new");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unmark_closes_conversation() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, _) = setup(&h, &tmp);
        let mut sse = open(&h, &a, &token).await;
        sse.entries().await;
        h.store().unmark("k", Some(1)).unwrap();
        let (ev, data) = sse.next().await.expect("closed event");
        assert_eq!(ev, "closed");
        assert_eq!(data, r#"{"reason":"unmarked"}"#);
        assert!(sse.next().await.is_none(), "stream ends after closed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_closes_on_revoke_and_stop() {
        let mut h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, _) = setup(&h, &tmp);

        let mut sse = open(&h, &a, &token).await;
        sse.entries().await;
        let other = h.device(false);
        let mut sse2 = open(&h, &a, &other).await;
        sse2.entries().await;

        let store = h.store();
        let dev = store.load().devices[0].id.clone();
        auth::revoke(&store, &dev).unwrap();
        let (ev, data) = sse.next().await.expect("closed event");
        assert_eq!(
            (ev.as_str(), data.as_str()),
            ("closed", r#"{"reason":"revoked"}"#)
        );

        let srv = h.server.take().unwrap();
        std::thread::spawn(move || srv.stop());
        let (ev, data) = sse2.next().await.expect("closed event");
        assert_eq!(
            (ev.as_str(), data.as_str()),
            ("closed", r#"{"reason":"disabled"}"#)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_closes_when_snapshot_drops_agent() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, _) = setup(&h, &tmp);
        let mut sse = open(&h, &a, &token).await;
        sse.entries().await;
        // Still marked, but the app no longer lists the id.
        h.set_agents(vec![]);
        let (ev, data) = sse.next().await.expect("closed event");
        assert_eq!(
            (ev.as_str(), data.as_str()),
            ("closed", r#"{"reason":"unmarked"}"#)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn conversation_waits_when_transcript_goes_away() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let (a, path) = setup(&h, &tmp);
        let mut sse = open(&h, &a, &token).await;
        let (_, first) = sse.entries().await;
        assert!(!first.is_empty());

        // Same id, still marked and listed, no transcript: empty, not closed.
        let mut none = conv_agent(&a, "claude-code", &path);
        none.transcript = None;
        h.set_agents(vec![none]);
        let (replace, entries) = sse.entries().await;
        assert!(replace);
        assert!(entries.is_empty(), "{entries:?}");

        // A path appears again: its entries replace the empty list.
        h.set_agents(vec![conv_agent(&a, "claude-code", &path)]);
        let (replace, entries) = sse.entries().await;
        assert!(replace);
        assert_eq!(ids(&entries), ids(&first));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tool_endpoint_caps_output() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(false);
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("big.jsonl");
        let big = "x".repeat(200 * 1024);
        let lines = [
            r#"{"type":"user","uuid":"u1","parentUuid":null,"message":{"role":"user","content":"go"}}"#.to_string(),
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"yes"}}]}}"#.to_string(),
            format!(
                r#"{{"type":"user","uuid":"u2","parentUuid":"a1","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t1","content":"{big}","is_error":false}}]}}}}"#
            ),
        ];
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        let a = h.store().mark("k", 1).unwrap();
        h.set_agents(vec![conv_agent(&a, "claude-code", &path)]);

        let r = h
            .get(
                &format!("/api/agents/{a}/conversation/tool/t1"),
                Some(&token),
            )
            .await;
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["text"].as_str().unwrap().len(), 64 * 1024);
        assert_eq!(v["truncated"], true);
        assert_eq!(v["is_error"], false);

        let r = h
            .get(
                &format!("/api/agents/{a}/conversation/tool/nope"),
                Some(&token),
            )
            .await;
        assert_eq!(r.status(), 404);
        assert_eq!(
            h.get(&format!("/api/agents/{a}/conversation/tool/t1"), None)
                .await
                .status(),
            401
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_only_device_can_read_conversation() {
        let h = Harness::start(Harness::unused_tmux());
        let token = h.device(true);
        let tmp = TempDir::new().unwrap();
        let (a, _) = setup(&h, &tmp);
        let mut sse = open(&h, &a, &token).await;
        let (replace, entries) = sse.entries().await;
        assert!(replace && !entries.is_empty());
        let r = h
            .get(
                &format!("/api/agents/{a}/conversation/tool/toolu_2"),
                Some(&token),
            )
            .await;
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["text"], "no match");
        assert_eq!(v["is_error"], true);
        assert_eq!(v["truncated"], false);
    }
}
