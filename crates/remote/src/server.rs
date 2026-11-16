//! HTTPS server lifecycle: binding, TLS (rustls with the `ring` provider),
//! certificate hot-reload, the shared state every route reads, and the
//! background loops (remote.json watcher, tmux liveness poll, agents view).
//!
//! The server owns its own multi-thread tokio runtime. `start` does blocking
//! work (scutil, key generation, file I/O), so callers keep it off the GPUI
//! main thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdListener};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use axum_server::tls_rustls::RustlsConfig;
use notify::RecommendedWatcher;
use serde::Serialize;
use tokio::sync::{watch, Notify};

use crate::certs;
use crate::tmux_bridge::{live_sessions, Hubs, Tmux};
use crate::{auth, RemoteFile, Store};

/// tmux liveness poll for `stopped`.
const LIVE_POLL: Duration = Duration::from_secs(2);
/// Backstop reload of remote.json in case a watcher event is missed.
const FILE_POLL: Duration = Duration::from_millis(500);
/// How long `stop` waits for sockets to send `disabled` and close.
const STOP_FLUSH: Duration = Duration::from_secs(3);
/// Pair attempts allowed per peer IP per minute.
const PAIR_PER_MINUTE: usize = 5;

/// One marked agent tab as the phone sees it. `session` never leaves the
/// server.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentInfo {
    pub id: String,
    pub title: String,
    pub agent_kind: Option<String>,
    pub project: String,
    pub worktree: String,
    /// `running|waiting|idle|unknown|stopped`.
    pub status: String,
    pub status_since: Option<String>,
    pub tool: Option<String>,
    #[serde(skip)]
    pub session: String,
}

/// What the app publishes: its marked tabs with their current status.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub agents: Vec<AgentInfo>,
}

/// The channel the app sends snapshots through.
pub fn snapshot_channel() -> (watch::Sender<Snapshot>, watch::Receiver<Snapshot>) {
    watch::channel(Snapshot::default())
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// `~/.openorchestra` (remote.json lives here, certs in `remote/`).
    pub state_dir: PathBuf,
    /// HTTPS port (7443); 0 picks a free port (tests).
    pub port: u16,
    /// Plain-HTTP setup port (7080), listening only while setup is open.
    pub setup_port: u16,
    pub tmux: Tmux,
    /// 0.0.0.0 in the app; tests use 127.0.0.1.
    pub bind_ip: IpAddr,
    /// Fixed cert SANs (tests) instead of LocalHostName + private IPv4s.
    pub hosts_override: Option<(Vec<String>, Vec<IpAddr>)>,
}

#[derive(Debug)]
pub enum StartError {
    PortInUse(u16),
    Other(String),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartError::PortInUse(p) => write!(f, "port {p} is already in use"),
            StartError::Other(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for StartError {}

fn other(e: impl fmt::Display) -> StartError {
    StartError::Other(e.to_string())
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerStatus {
    /// Phone-usable URLs (`https://<name>.local:<port>/`, `https://<ip>:<port>/`).
    pub urls: Vec<String>,
    pub ca_fingerprint: String,
    /// Devices paired on an IP this Mac no longer has.
    pub stale_ip_devices: Vec<String>,
}

/// CA artifacts served on the setup port.
pub(crate) struct CaInfo {
    pub pem: String,
    pub fingerprint: String,
    pub mobileconfig: String,
}

/// State shared by every route and background loop.
pub(crate) struct Shared {
    pub state_dir: PathBuf,
    pub tmux: Tmux,
    pub hubs: Hubs,
    pub port: u16,
    pub snapshot: watch::Receiver<Snapshot>,
    /// Latest remote.json (pushed to open sockets on change).
    pub file: watch::Sender<Arc<RemoteFile>>,
    /// Session names alive on the tmux server.
    pub live: watch::Sender<Arc<HashSet<String>>>,
    /// The deduped list SSE clients receive.
    pub agents: watch::Sender<Arc<Vec<AgentInfo>>>,
    /// `true` once `stop` was called.
    pub shutdown: watch::Sender<bool>,
    /// SAN hosts/IPs of the certificate currently served.
    pub sans: RwLock<(Vec<String>, Vec<IpAddr>)>,
    pub ca: CaInfo,
    pub status: RwLock<ServerStatus>,
    /// One clone per open WebSocket/SSE stream (for `stop`'s flush wait).
    pub open_streams: Arc<()>,
    pair_attempts: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    hosts_override: Mutex<Option<(Vec<String>, Vec<IpAddr>)>>,
    pub(crate) cert_wake: Notify,
}

impl Shared {
    pub fn store(&self) -> Store {
        Store::new(&self.state_dir)
    }

    /// The file as it is on disk now (request paths never trust a cache).
    pub fn current_file(&self) -> RemoteFile {
        self.store().load()
    }

    /// Phone-visible agents given `file`'s marks.
    pub fn agents_for(&self, file: &RemoteFile) -> Vec<AgentInfo> {
        compute_agents(&self.snapshot.borrow(), file, &self.live.borrow())
    }

    /// Session behind `id`, only if `id` is currently marked in `file` and
    /// published by the app.
    pub fn session_for(&self, file: &RemoteFile, id: &str) -> Option<String> {
        file.mark_by_id(id)?;
        self.snapshot
            .borrow()
            .agents
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.session.clone())
    }

    /// Whether `origin` is exactly `https://<SAN>:<port>` for the cert in use.
    pub fn origin_allowed(&self, origin: &str) -> bool {
        let sans = self.sans.read().unwrap_or_else(|e| e.into_inner());
        let port = self.port;
        let mut ips = sans.1.clone();
        ips.push(IpAddr::V4(Ipv4Addr::LOCALHOST));
        sans.0
            .iter()
            .map(|h| format!("https://{}:{port}", h.to_ascii_lowercase()))
            .chain(ips.iter().map(|ip| match ip {
                IpAddr::V4(v4) => format!("https://{v4}:{port}"),
                IpAddr::V6(v6) => format!("https://[{v6}]:{port}"),
            }))
            .any(|o| o.eq_ignore_ascii_case(origin))
    }

    /// Count a pairing attempt from `ip`; false beyond 5 per minute.
    pub fn pair_attempt_allowed(&self, ip: IpAddr, now: Instant) -> bool {
        let mut map = self.pair_attempts.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, q| {
            q.retain(|t| now.duration_since(*t) < Duration::from_secs(60));
            !q.is_empty()
        });
        let q = map.entry(ip.to_canonical()).or_default();
        if q.len() >= PAIR_PER_MINUTE {
            return false;
        }
        q.push_back(now);
        true
    }

    fn reload_file(&self) {
        let fresh = self.current_file();
        self.file.send_if_modified(|cur| {
            if **cur == fresh {
                false
            } else {
                *cur = Arc::new(fresh);
                true
            }
        });
    }

    fn refresh_agents(&self) {
        let list = self.agents_for(&self.file.borrow());
        self.agents.send_if_modified(|cur| {
            if **cur == list {
                false
            } else {
                *cur = Arc::new(list);
                true
            }
        });
    }

    pub(crate) fn refresh_status(&self, current_ips: &[IpAddr]) {
        let (hosts, ips) = self.sans.read().unwrap_or_else(|e| e.into_inner()).clone();
        let port = self.port;
        let mut urls: Vec<String> = hosts
            .iter()
            .filter(|h| !h.eq_ignore_ascii_case("localhost"))
            .map(|h| format!("https://{h}:{port}/"))
            .collect();
        urls.extend(
            ips.iter()
                .filter(|ip| !ip.is_loopback())
                .map(|ip| match ip {
                    IpAddr::V4(v4) => format!("https://{v4}:{port}/"),
                    IpAddr::V6(v6) => format!("https://[{v6}]:{port}/"),
                }),
        );
        let stale = auth::devices_on_stale_ip(&self.file.borrow(), current_ips);
        let mut st = self.status.write().unwrap_or_else(|e| e.into_inner());
        st.urls = urls;
        st.stale_ip_devices = stale;
    }

    pub(crate) fn desired_sans(&self) -> (Vec<String>, Vec<IpAddr>) {
        if let Some(o) = self
            .hosts_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            return o;
        }
        (
            certs::san_hosts(certs::local_host_name().as_deref()),
            certs::private_ipv4s(),
        )
    }
}

/// Marked agents from the snapshot (first occurrence of each id), with
/// `stopped` forced for sessions that aren't alive.
pub fn compute_agents(
    snap: &Snapshot,
    file: &RemoteFile,
    live: &HashSet<String>,
) -> Vec<AgentInfo> {
    let mut seen = HashSet::new();
    snap.agents
        .iter()
        .filter(|a| file.mark_by_id(&a.id).is_some() && seen.insert(a.id.clone()))
        .map(|a| {
            let mut a = a.clone();
            if !live.contains(&a.session) {
                a.status = "stopped".into();
                a.tool = None;
            }
            a
        })
        .collect()
}

pub struct RemoteServer {
    shared: Arc<Shared>,
    rt: Option<tokio::runtime::Runtime>,
    https: axum_server::Handle<SocketAddr>,
    https_done: Option<std::sync::mpsc::Receiver<()>>,
    setup: Mutex<Option<axum_server::Handle<SocketAddr>>>,
    bind_ip: IpAddr,
    setup_port: u16,
    _watcher: Option<RecommendedWatcher>,
}

impl RemoteServer {
    pub fn start(
        cfg: ServerConfig,
        snapshot: watch::Receiver<Snapshot>,
    ) -> Result<RemoteServer, StartError> {
        let listener = StdListener::bind((cfg.bind_ip, cfg.port)).map_err(|e| {
            if e.kind() == io::ErrorKind::AddrInUse {
                StartError::PortInUse(cfg.port)
            } else {
                other(e)
            }
        })?;
        let port = listener.local_addr().map_err(other)?.port();

        let cert_dir = cfg.state_dir.join("remote");
        let ca =
            Arc::new(certs::load_or_create_ca(&cert_dir, &certs::computer_name()).map_err(other)?);
        let store = Store::new(&cfg.state_dir);
        let file0 = store.load();

        let shared = Arc::new(Shared {
            state_dir: cfg.state_dir.clone(),
            tmux: cfg.tmux.clone(),
            hubs: Hubs::new(cfg.tmux.clone()),
            port,
            snapshot,
            file: watch::channel(Arc::new(file0)).0,
            live: watch::channel(Arc::new(HashSet::new())).0,
            agents: watch::channel(Arc::new(Vec::new())).0,
            shutdown: watch::channel(false).0,
            sans: RwLock::new((Vec::new(), Vec::new())),
            ca: CaInfo {
                pem: ca.cert_pem.clone(),
                fingerprint: certs::fingerprint_sha256(&ca.cert_der),
                mobileconfig: certs::mobileconfig(&ca),
            },
            status: RwLock::new(ServerStatus::default()),
            open_streams: Arc::new(()),
            pair_attempts: Mutex::new(HashMap::new()),
            hosts_override: Mutex::new(cfg.hosts_override.clone()),
            cert_wake: Notify::new(),
        });
        shared.status.write().unwrap().ca_fingerprint = shared.ca.fingerprint.clone();

        let (hosts, ips) = shared.desired_sans();
        let cert =
            certs::load_or_issue_server_cert(&cert_dir, &ca, &hosts, &ips, SystemTime::now())
                .map_err(other)?;
        let tls =
            RustlsConfig::from_config(Arc::new(crate::tls::tls_config(&cert).map_err(other)?));
        *shared.sans.write().unwrap() = (cert.hosts.clone(), cert.ips.clone());
        shared.refresh_status(&ips);

        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("oo-remote")
            .enable_all()
            .build()
            .map_err(other)?;

        // First liveness poll before serving, so a fresh server never lists
        // live agents as stopped.
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        {
            let s = shared.clone();
            rt.spawn(async move {
                let live = live_sessions(&s.tmux).await;
                s.live.send_replace(Arc::new(live));
                s.refresh_agents();
                let _ = ready_tx.send(());
            });
        }
        let _ = ready_rx.recv_timeout(Duration::from_secs(5));

        let (watcher, file_events) = match store.watch() {
            Ok((w, rx)) => (Some(w), Some(rx)),
            Err(e) => {
                eprintln!("oo-remote: watching remote.json failed ({e}); polling only");
                (None, None)
            }
        };
        let file_wake = Arc::new(Notify::new());
        if let Some(rx) = file_events {
            let wake = file_wake.clone();
            // Exits when the watcher (and with it the sender) is dropped.
            std::thread::Builder::new()
                .name("oo-remote-watch".into())
                .spawn(move || {
                    while rx.recv().is_ok() {
                        wake.notify_one();
                    }
                })
                .map_err(other)?;
        }

        rt.spawn(file_loop(shared.clone(), file_wake));
        rt.spawn(live_loop(shared.clone()));
        rt.spawn(agents_loop(shared.clone()));
        rt.spawn(crate::tls::cert_loop(
            shared.clone(),
            ca,
            cert,
            tls.clone(),
            cert_dir,
        ));

        let https = axum_server::Handle::new();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        {
            let app = crate::api::router(shared.clone());
            let handle = https.clone();
            rt.spawn(async move {
                let result = match axum_server::tls_rustls::from_tcp_rustls(listener, tls) {
                    Ok(server) => {
                        server
                            .handle(handle)
                            .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                            .await
                    }
                    Err(e) => Err(e),
                };
                if let Err(e) = result {
                    eprintln!("oo-remote: https server ended: {e}");
                }
                let _ = done_tx.send(());
            });
        }

        Ok(RemoteServer {
            shared,
            rt: Some(rt),
            https,
            https_done: Some(done_rx),
            setup: Mutex::new(None),
            bind_ip: cfg.bind_ip,
            setup_port: cfg.setup_port,
            _watcher: watcher,
        })
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    pub fn status(&self) -> ServerStatus {
        self.shared
            .status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Open or close the plain-HTTP setup port. Opening binds before it
    /// returns; a bind failure is logged and leaves it closed.
    pub fn set_setup_open(&self, open: bool) {
        let mut slot = self.setup.lock().unwrap_or_else(|e| e.into_inner());
        if !open {
            if let Some(h) = slot.take() {
                h.shutdown();
            }
            return;
        }
        if slot.is_some() {
            return;
        }
        let Some(rt) = &self.rt else { return };
        let listener = match StdListener::bind((self.bind_ip, self.setup_port)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("oo-remote: setup port {}: {e}", self.setup_port);
                return;
            }
        };
        let handle = axum_server::Handle::new();
        let app = crate::setup::router(self.shared.clone());
        let h = handle.clone();
        rt.spawn(async move {
            let result = match axum_server::from_tcp(listener) {
                Ok(server) => {
                    server
                        .handle(h)
                        .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                        .await
                }
                Err(e) => Err(e),
            };
            if let Err(e) = result {
                eprintln!("oo-remote: setup server ended: {e}");
            }
        });
        *slot = Some(handle);
    }

    /// Replace the cert SAN set and re-check the cert now (tests).
    #[doc(hidden)]
    pub fn test_set_hosts(&self, hosts: Vec<String>, ips: Vec<IpAddr>) {
        *self
            .shared
            .hosts_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((hosts, ips));
        self.shared.cert_wake.notify_one();
    }

    /// Send `disabled` to every socket, close everything and free the ports.
    pub fn stop(mut self) {
        self.shutdown_now();
    }

    fn shutdown_now(&mut self) {
        let Some(rt) = self.rt.take() else { return };
        self.shared.shutdown.send_replace(true);
        let deadline = Instant::now() + STOP_FLUSH;
        while Arc::strong_count(&self.shared.open_streams) > 1 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(h) = self.setup.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.shutdown();
        }
        self.https
            .graceful_shutdown(Some(Duration::from_millis(500)));
        if let Some(done) = self.https_done.take() {
            let _ = done.recv_timeout(Duration::from_secs(2));
        }
        // Never blocks (safe from inside another runtime).
        rt.shutdown_background();
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.shutdown_now();
    }
}

/// Resolves once `stop` was requested (never holds the watch guard).
pub(crate) async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|v| *v).await;
}

async fn file_loop(s: Arc<Shared>, wake: Arc<Notify>) {
    let mut stop = s.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            _ = wake.notified() => {}
            _ = tokio::time::sleep(FILE_POLL) => {}
        }
        let s2 = s.clone();
        let _ = tokio::task::spawn_blocking(move || s2.reload_file()).await;
    }
}

async fn live_loop(s: Arc<Shared>) {
    let mut stop = s.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            _ = tokio::time::sleep(LIVE_POLL) => {}
        }
        let live = live_sessions(&s.tmux).await;
        s.live.send_if_modified(|cur| {
            if **cur == live {
                false
            } else {
                *cur = Arc::new(live);
                true
            }
        });
    }
}

/// Recompute the SSE agents list whenever its inputs change.
async fn agents_loop(s: Arc<Shared>) {
    let mut stop = s.shutdown.subscribe();
    let mut snap = s.snapshot.clone();
    let mut file = s.file.subscribe();
    let mut live = s.live.subscribe();
    let mut snap_open = true;
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            r = snap.changed(), if snap_open => {
                // The app dropped its sender: keep serving the last snapshot.
                snap_open = r.is_ok();
            }
            _ = file.changed() => {}
            _ = live.changed() => {}
        }
        s.refresh_agents();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mark;

    fn info(id: &str, session: &str) -> AgentInfo {
        AgentInfo {
            id: id.into(),
            title: id.into(),
            agent_kind: None,
            project: "p".into(),
            worktree: "w".into(),
            status: "waiting".into(),
            status_since: Some("2026-01-01T00:00:00Z".into()),
            tool: Some("Bash".into()),
            session: session.into(),
        }
    }

    #[test]
    fn agents_only_marked_deduped_and_stopped_when_dead() {
        let file = RemoteFile {
            marked: vec![
                Mark {
                    worktree_key: "k".into(),
                    tab_id: 1,
                    id: "a".into(),
                },
                Mark {
                    worktree_key: "k".into(),
                    tab_id: 2,
                    id: "b".into(),
                },
            ],
            ..RemoteFile::default()
        };
        let snap = Snapshot {
            agents: vec![
                info("a", "sa"),
                info("b", "sb"),
                info("c", "sc"),
                info("a", "sx"),
            ],
        };
        let live: HashSet<String> = ["sa".to_string()].into();
        let out = compute_agents(&snap, &file, &live);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[0].status, "waiting");
        assert_eq!(out[0].tool.as_deref(), Some("Bash"));
        assert_eq!(out[1].id, "b");
        assert_eq!(out[1].status, "stopped");
        assert_eq!(out[1].tool, None);
        let json = serde_json::to_value(&out[0]).unwrap();
        assert!(json.get("session").is_none());
    }
}
