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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime};

use axum_server::tls_rustls::RustlsConfig;
use notify::RecommendedWatcher;
use serde::Serialize;
use tokio::sync::{watch, Notify};

use crate::certs;
use crate::conversation::TranscriptKind;
use crate::tmux_bridge::{live_sessions, Hubs, Tmux};
use crate::transcript::Transcripts;
use crate::{auth, RemoteFile, Store};

/// tmux liveness poll for `stopped`.
const LIVE_POLL: Duration = Duration::from_secs(2);
/// How often the server re-reads this Mac's networks.
const NETWORK_POLL: Duration = Duration::from_secs(10);
/// Shell-log housekeeping every this many liveness polls (about a minute).
const SHELL_GC_POLLS: u32 = 30;
/// Backstop reload of remote.json in case a watcher event is missed.
const FILE_POLL: Duration = Duration::from_millis(500);
/// How long `stop` waits for sockets to send `disabled` and close.
const STOP_FLUSH: Duration = Duration::from_secs(3);
/// Terminal sockets: server ping period, and how long without any frame
/// from the phone (pongs included) before the socket is dropped. A phone
/// that left Wi-Fi must not hold "Fit to screen" forever.
const WS_PING: Duration = Duration::from_secs(20);
const WS_IDLE: Duration = Duration::from_secs(45);
/// Pair attempts allowed per peer IP per minute.
const PAIR_PER_MINUTE: usize = 5;
/// How long the Mac shows that a device typed into a tab.
const INPUT_SHOWN_FOR: Duration = Duration::from_secs(8);
/// Wrong codes from one address before it is locked out of pairing.
const PAIR_FAILURES_PER_ADDRESS: u32 = 5;
/// How long a locked address stays locked after its last wrong code.
const PAIR_LOCK: Duration = Duration::from_secs(10 * 60);

/// Wrong pairing codes per address, and when the last one came.
#[derive(Default)]
pub(crate) struct AddressLocks(HashMap<IpAddr, (u32, Instant)>);

impl AddressLocks {
    fn locked(&mut self, ip: IpAddr, now: Instant) -> bool {
        self.0
            .retain(|_, (_, last)| now.duration_since(*last) < PAIR_LOCK);
        self.0
            .get(&ip.to_canonical())
            .is_some_and(|(n, _)| *n >= PAIR_FAILURES_PER_ADDRESS)
    }

    fn record(&mut self, ip: IpAddr, ok: bool, now: Instant) {
        let ip = ip.to_canonical();
        if ok {
            self.0.remove(&ip);
        } else {
            let e = self.0.entry(ip).or_insert((0, now));
            e.0 += 1;
            e.1 = now;
        }
    }
}

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
    /// Git branch of the tab's worktree (filled by the app).
    pub branch: Option<String>,
    /// Whether `/api/agents/{id}/conversation` serves this agent. Computed
    /// by the server from `transcript` and `agent_kind`.
    pub has_conversation: bool,
    #[serde(skip)]
    pub session: String,
    /// The agent's transcript file. Never leaves the server.
    #[serde(skip)]
    pub transcript: Option<PathBuf>,
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
    /// The LAN address to hand phones (setup QR, Android link): the
    /// default-route interface's private IPv4, when the cert covers it.
    pub primary_ip: Option<IpAddr>,
    /// Why the setup port ("Add a device") could not be opened, for the UI.
    /// Cleared when the port opens or is closed.
    pub setup_error: Option<String>,
    /// Devices that typed into a session in the last few seconds.
    pub remote_input: Vec<crate::activity::RemoteInput>,
    /// Networks this Mac is on, and trusted ones it isn't on.
    pub networks: Vec<NetworkStatus>,
    /// This Mac's name or an address changed since its CA was made: devices
    /// using those can't connect until a new CA is made and trusted.
    pub ca_outdated: bool,
}

/// One network for Settings: whether the Mac is on it and whether devices
/// may connect from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetworkStatus {
    pub key: String,
    pub here: bool,
    pub primary: bool,
    pub trusted: bool,
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
    /// Transcript tailers shared by conversation viewers.
    pub transcripts: Transcripts,
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
    /// This Mac's name (`ComputerName`), shown on the phone so two Macs'
    /// home-screen apps can be told apart. Only the embedded web client reads
    /// it, so a build without `web/remote/dist` (CI's headless job) sees it
    /// unread.
    #[cfg_attr(oo_remote_no_web, allow(dead_code))]
    pub host_name: String,
    pub status: RwLock<ServerStatus>,
    /// One clone per open WebSocket/SSE stream (for `stop`'s flush wait).
    pub open_streams: Arc<()>,
    pair_attempts: Mutex<HashMap<IpAddr, VecDeque<Instant>>>,
    /// Wrong pairing codes per address, and when the last one came.
    pair_failures: Mutex<AddressLocks>,
    /// Who typed where from a device (see `crate::activity`).
    pub(crate) input: crate::activity::InputActivity,
    /// This Mac's networks, refreshed off the request path.
    networks: RwLock<Vec<crate::network::LocalNet>>,
    /// Keys of the networks devices may connect from.
    trusted: RwLock<HashSet<String>>,
    hosts_override: Mutex<Option<(Vec<String>, Vec<IpAddr>)>>,
    pub(crate) cert_wake: Notify,
    /// Cert recheck period (60s; shorter in tests).
    pub(crate) cert_check: Duration,
    /// Test hook: pause between opening the hub and typing in `send`.
    pub(crate) test_send_delay_ms: AtomicU64,
    /// Terminal socket ping period / idle limit, in ms (shorter in tests).
    ws_ping_ms: AtomicU64,
    ws_idle_ms: AtomicU64,
    /// A bad remote.json read was logged (once).
    bad_file_logged: AtomicBool,
}

impl Shared {
    /// (ping period, idle limit) for terminal sockets.
    pub(crate) fn ws_keepalive(&self) -> (Duration, Duration) {
        (
            Duration::from_millis(self.ws_ping_ms.load(Ordering::SeqCst)),
            Duration::from_millis(self.ws_idle_ms.load(Ordering::SeqCst)),
        )
    }

    pub fn store(&self) -> Store {
        Store::new(&self.state_dir)
    }

    /// The file as it is on disk now (request paths never trust a cache).
    /// An unreadable or unparseable file keeps the last good copy: a
    /// transient bad read must not look like "every device revoked" (phones
    /// would get 401 / `revoked` and drop their token).
    pub fn current_file(&self) -> RemoteFile {
        match self.store().try_load() {
            Some(f) => f,
            None => {
                if !self.bad_file_logged.swap(true, Ordering::SeqCst) {
                    eprintln!("oo-remote: remote.json unreadable; keeping the last good copy");
                }
                (**self.file.borrow()).clone()
            }
        }
    }

    /// Phone-visible agents given `file`'s marks.
    pub fn agents_for(&self, file: &RemoteFile) -> Vec<AgentInfo> {
        compute_agents(
            &self.snapshot.borrow(),
            file,
            &self.live.borrow(),
            &self.state_dir,
        )
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

    /// Transcript behind `id`, only if `id` is currently marked in `file`
    /// and the app published a supported transcript for it. The path never
    /// leaves the server.
    pub fn transcript_for(&self, file: &RemoteFile, id: &str) -> Option<(PathBuf, TranscriptKind)> {
        file.mark_by_id(id)?;
        let snap = self.snapshot.borrow();
        let a = snap.agents.iter().find(|a| a.id == id)?;
        let kind = transcript_kind(a)?;
        Some((transcript_path(a, &self.state_dir)?, kind))
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
    /// Whether `ip` is locked out of pairing: `PAIR_FAILURES_PER_ADDRESS`
    /// wrong codes within `PAIR_LOCK`. Only that address is stopped; the
    /// code on show stays valid for everyone else.
    pub fn pair_locked(&self, ip: IpAddr, now: Instant) -> bool {
        self.pair_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .locked(ip, now)
    }

    /// Record a pairing result from `ip`: a wrong code counts toward its
    /// lock, a right one clears it.
    pub fn pair_result(&self, ip: IpAddr, ok: bool, now: Instant) {
        self.pair_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .record(ip, ok, now);
    }

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
        self.sync_shell_flags(&list);
        self.agents.send_if_modified(|cur| {
            if **cur == list {
                false
            } else {
                *cur = Arc::new(list);
                true
            }
        });
    }

    /// Whether the CA no longer covers what devices use to reach this Mac:
    /// its `.local` name or its main address. Other addresses (a VM bridge,
    /// a second network) don't count: nagging about those would push the
    /// user to replace a CA every device trusts. Blocking (`route`).
    pub(crate) fn ca_outdated(&self, ca: &certs::Ca, hosts: &[String], ips: &[IpAddr]) -> bool {
        let names: Vec<String> = hosts
            .iter()
            .filter(|h| h.to_ascii_lowercase().ends_with(".local"))
            .cloned()
            .collect();
        let primary: Vec<IpAddr> = self.desired_primary(ips).into_iter().collect();
        ca.outdated_for(&names, &primary)
    }

    /// Whether `ip` is on a network the user trusted (loopback always is).
    pub(crate) fn peer_trusted(&self, ip: IpAddr) -> bool {
        let nets = self.networks.read().unwrap_or_else(|e| e.into_inner());
        let trusted = self.trusted.read().unwrap_or_else(|e| e.into_inner());
        crate::network::peer_trusted(ip, &nets, &trusted)
    }

    /// Re-read the trusted list and, when given, this Mac's networks; then
    /// the networks in the status. Blocking when `nets` was just computed.
    fn set_networks(&self, nets: Option<Vec<crate::network::LocalNet>>) {
        if let Some(nets) = nets {
            let mut cur = self.networks.write().unwrap_or_else(|e| e.into_inner());
            *cur = crate::network::keep_routers(&cur, nets);
        }
        // An unreadable file keeps the last list rather than trusting none
        // (or all) for a moment.
        if let Some(sec) = crate::security::load(&self.state_dir) {
            *self.trusted.write().unwrap_or_else(|e| e.into_inner()) =
                sec.trusted_networks.into_iter().map(|t| t.key).collect();
        }
        let nets = self.networks.read().unwrap_or_else(|e| e.into_inner());
        let trusted = self.trusted.read().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<NetworkStatus> = nets
            .iter()
            .map(|n| NetworkStatus {
                key: n.key(),
                here: true,
                primary: n.primary,
                trusted: trusted.contains(&n.key()),
            })
            .collect();
        let mut away: Vec<&String> = trusted
            .iter()
            .filter(|k| !list.iter().any(|n| &n.key == *k))
            .collect();
        away.sort();
        list.extend(away.into_iter().map(|k| NetworkStatus {
            key: k.clone(),
            here: false,
            primary: false,
            trusted: true,
        }));
        self.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .networks = list;
    }

    /// Let exactly the shared shell tabs record commands (`crate::shell`).
    fn sync_shell_flags(&self, list: &[AgentInfo]) {
        let shared: HashSet<String> = list
            .iter()
            .filter(|a| a.agent_kind.is_none())
            .map(|a| a.session.clone())
            .collect();
        if let Err(e) = crate::shell::sync_flags(&self.state_dir, &shared) {
            eprintln!("oo-remote: shell logging switches: {e}");
        }
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

    /// The address for the setup QR / Android link among `ips` (the SAN
    /// IPs). Blocking (`route`): call off the UI thread.
    pub(crate) fn desired_primary(&self, ips: &[IpAddr]) -> Option<IpAddr> {
        let first = || ips.iter().copied().find(|ip| !ip.is_loopback());
        let overridden = self
            .hosts_override
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some();
        if overridden {
            return first();
        }
        certs::primary_ipv4()
            .filter(|p| ips.contains(p))
            .or_else(first)
    }

    pub(crate) fn set_primary(&self, primary: Option<IpAddr>) {
        self.status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .primary_ip = primary;
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

/// How `a`'s transcript reads: by agent kind, or a shell's command log.
fn transcript_kind(a: &AgentInfo) -> Option<TranscriptKind> {
    match a.agent_kind.as_deref() {
        Some(k) => TranscriptKind::from_agent_kind(k),
        None => Some(TranscriptKind::Shell),
    }
}

/// The first start of a build with trusted networks: with devices already
/// paired, trust the network the Mac is on, so they keep working. Runs once;
/// a corrupt file is left alone (and nothing is trusted).
fn migrate_trust(state_dir: &Path, has_devices: bool, nets: &[crate::network::LocalNet]) {
    let result = crate::security::update(state_dir, |f| {
        if f.migrated {
            return;
        }
        if !has_devices {
            f.migrated = true;
            return;
        }
        // Offline at launch: try again next start rather than locking the
        // existing devices out for good.
        if let Some(n) = nets.iter().find(|n| n.primary) {
            f.migrated = true;
            eprintln!(
                "oo-remote: trusting this network for devices paired before: {}",
                n.key()
            );
            f.trusted_networks.push(crate::network::TrustedNet {
                key: n.key(),
                added: rfc3339_now(),
            });
        }
    });
    if let Err(e) = result {
        eprintln!("oo-remote: security settings unreadable ({e}); no network is trusted");
    }
}

fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// `a`'s transcript: the one the app found, or for a shell tab its command
/// log (named after its tmux session; see `crate::shell`).
fn transcript_path(a: &AgentInfo, state_dir: &Path) -> Option<PathBuf> {
    match (&a.transcript, a.agent_kind.as_deref()) {
        (Some(t), _) => Some(t.clone()),
        (None, None) => Some(crate::shell::log_path(state_dir, &a.session)),
        (None, Some(_)) => None,
    }
}

/// Marked agents from the snapshot (first occurrence of each id), with
/// `stopped` forced for sessions that aren't alive.
pub fn compute_agents(
    snap: &Snapshot,
    file: &RemoteFile,
    live: &HashSet<String>,
    state_dir: &Path,
) -> Vec<AgentInfo> {
    let mut seen = HashSet::new();
    snap.agents
        .iter()
        .filter(|a| file.mark_by_id(&a.id).is_some() && seen.insert(a.id.clone()))
        .map(|a| {
            let mut a = a.clone();
            a.transcript = transcript_path(&a, state_dir);
            a.has_conversation = match (&a.transcript, transcript_kind(&a)) {
                // A shell's log exists only once its hook ran (zsh tabs
                // started by this build); otherwise it is Terminal-only.
                (Some(t), Some(TranscriptKind::Shell)) => t.is_file(),
                (Some(_), Some(_)) => true,
                _ => false,
            };
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
        Self::start_with_cert_check(cfg, snapshot, crate::tls::CERT_CHECK)
    }

    /// `start` with a custom cert recheck period (tests).
    #[doc(hidden)]
    pub fn start_with_cert_check(
        cfg: ServerConfig,
        snapshot: watch::Receiver<Snapshot>,
        cert_check: Duration,
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
        let host_name = certs::computer_name();
        // This Mac's names and addresses now: a new CA vouches for these only.
        let (hosts0, ips0) = cfg.hosts_override.clone().unwrap_or_else(|| {
            (
                certs::san_hosts(certs::local_host_name().as_deref()),
                certs::private_ipv4s(),
            )
        });
        let ca = Arc::new(
            certs::load_or_create_ca(&cert_dir, &host_name, &hosts0, &ips0).map_err(other)?,
        );
        let store = Store::new(&cfg.state_dir);
        let file0 = store.load();
        let nets = crate::network::local_networks();
        migrate_trust(&cfg.state_dir, !file0.devices.is_empty(), &nets);

        let shared = Arc::new(Shared {
            state_dir: cfg.state_dir.clone(),
            tmux: cfg.tmux.clone(),
            hubs: Hubs::new(cfg.tmux.clone()),
            transcripts: Transcripts::new(),
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
            host_name,
            status: RwLock::new(ServerStatus::default()),
            open_streams: Arc::new(()),
            pair_attempts: Mutex::new(HashMap::new()),
            pair_failures: Mutex::new(AddressLocks::default()),
            input: crate::activity::InputActivity::new(&cfg.state_dir),
            networks: RwLock::new(nets),
            trusted: RwLock::new(HashSet::new()),
            hosts_override: Mutex::new(cfg.hosts_override.clone()),
            cert_wake: Notify::new(),
            cert_check,
            test_send_delay_ms: AtomicU64::new(0),
            ws_ping_ms: AtomicU64::new(WS_PING.as_millis() as u64),
            ws_idle_ms: AtomicU64::new(WS_IDLE.as_millis() as u64),
            bad_file_logged: AtomicBool::new(false),
        });
        shared.status.write().unwrap().ca_fingerprint = shared.ca.fingerprint.clone();
        shared.set_networks(None);

        let (want_hosts, ips) = shared.desired_sans();
        shared.status.write().unwrap().ca_outdated = shared.ca_outdated(&ca, &want_hosts, &ips);
        let (hosts, covered_ips) = ca.covered(&want_hosts, &ips);
        let cert = certs::load_or_issue_server_cert(
            &cert_dir,
            &ca,
            &hosts,
            &covered_ips,
            SystemTime::now(),
        )
        .map_err(other)?;
        let tls =
            RustlsConfig::from_config(Arc::new(crate::tls::tls_config(&cert).map_err(other)?));
        *shared.sans.write().unwrap() = (cert.hosts.clone(), cert.ips.clone());
        shared.set_primary(shared.desired_primary(&cert.ips));
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
        rt.spawn(network_loop(shared.clone()));
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
        let mut st = self
            .shared
            .status
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        st.remote_input = self.shared.input.recent(SystemTime::now(), INPUT_SHOWN_FOR);
        st
    }

    /// Open or close the plain-HTTP setup port. Opening binds before it
    /// returns; a bind failure leaves it closed and is reported in
    /// [`ServerStatus::setup_error`] until the next open or close.
    pub fn set_setup_open(&self, open: bool) {
        let mut slot = self.setup.lock().unwrap_or_else(|e| e.into_inner());
        if !open {
            if let Some(h) = slot.take() {
                h.shutdown();
            }
            self.set_setup_error(None);
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
                self.set_setup_error(Some(format!(
                    "Couldn't open port {} for phone setup \u{2014} is another app using it?",
                    self.setup_port
                )));
                return;
            }
        };
        self.set_setup_error(None);
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

    fn set_setup_error(&self, error: Option<String>) {
        self.shared
            .status
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .setup_error = error;
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

    /// Replace the cert SAN set without waking the cert loop: it is picked
    /// up on the next periodic check (tests).
    #[doc(hidden)]
    pub fn test_set_hosts_quiet(&self, hosts: Vec<String>, ips: Vec<IpAddr>) {
        *self
            .shared
            .hosts_override
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((hosts, ips));
    }

    /// Pause `send` between opening the session and typing (tests).
    #[doc(hidden)]
    pub fn test_delay_send_after_open(&self, delay: Duration) {
        self.shared
            .test_send_delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }

    /// Terminal socket ping period and idle limit for sockets opened from now
    /// on (tests).
    #[doc(hidden)]
    pub fn test_set_ws_keepalive(&self, ping: Duration, idle: Duration) {
        self.shared
            .ws_ping_ms
            .store(ping.as_millis() as u64, Ordering::SeqCst);
        self.shared
            .ws_idle_ms
            .store(idle.as_millis() as u64, Ordering::SeqCst);
    }

    /// Quit path: stop accepting fits and put every fitted window back to
    /// its saved size now, blocking for at most `budget` (spawns nothing
    /// when no fit is active). tmux sessions outlive the app, so a window
    /// left at phone size by a stop thread the exiting process cut short
    /// would stay that way. Callable from any non-tokio thread.
    pub fn restore_fits_now(&self, budget: Duration) {
        self.shared.shutdown.send_replace(true);
        self.shared.hubs.restore_fits_blocking(budget);
    }

    /// Let devices connect from the network with this key (see
    /// [`NetworkStatus::key`]).
    pub fn trust_network(&self, key: &str) -> io::Result<()> {
        crate::security::update(&self.shared.state_dir, |f| {
            if !f.trusted_networks.iter().any(|t| t.key == key) {
                f.trusted_networks.push(crate::network::TrustedNet {
                    key: key.to_string(),
                    added: rfc3339_now(),
                });
            }
        })?;
        self.shared.set_networks(None);
        Ok(())
    }

    /// Stop devices connecting from the network with this key.
    pub fn forget_network(&self, key: &str) -> io::Result<()> {
        crate::security::update(&self.shared.state_dir, |f| {
            f.trusted_networks.retain(|t| t.key != key);
        })?;
        self.shared.set_networks(None);
        Ok(())
    }

    /// Trust the network the default route goes through, if any (opening
    /// "Add a device" there). Returns its key.
    pub fn trust_primary_network(&self) -> io::Result<Option<String>> {
        let key = self
            .shared
            .networks
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|n| n.primary)
            .map(|n| n.key());
        if let Some(k) = &key {
            self.trust_network(k)?;
        }
        Ok(key)
    }

    /// Send `disabled` to every socket, close everything and free the ports.
    pub fn stop(mut self) {
        self.shutdown_now();
    }

    fn shutdown_now(&mut self) {
        let Some(rt) = self.rt.take() else { return };
        self.shared.shutdown.send_replace(true);
        // Remote is off: no shell tab records commands any more.
        if let Err(e) = crate::shell::sync_flags(&self.shared.state_dir, &HashSet::new()) {
            eprintln!("oo-remote: shell logging switches: {e}");
        }
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

/// Keep this Mac's networks and the trusted list current (Wi-Fi changes,
/// trust changed by the other build).
async fn network_loop(s: Arc<Shared>) {
    let mut stop = s.shutdown.subscribe();
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            _ = tokio::time::sleep(NETWORK_POLL) => {}
        }
        let s2 = s.clone();
        let _ = tokio::task::spawn_blocking(move || {
            s2.set_networks(Some(crate::network::local_networks()));
        })
        .await;
    }
}

async fn live_loop(s: Arc<Shared>) {
    let mut stop = s.shutdown.subscribe();
    let mut polls: u32 = 0;
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => return,
            _ = tokio::time::sleep(LIVE_POLL) => {}
        }
        // Shell-log housekeeping, about once a minute, and only from a
        // listing that worked (a failed one would look like "all closed").
        polls = polls.wrapping_add(1);
        if polls.is_multiple_of(SHELL_GC_POLLS) {
            if let Ok(live) = crate::tmux_bridge::try_live_sessions(&s.tmux).await {
                let dir = s.state_dir.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = crate::shell::collect(&dir, &live, SystemTime::now()) {
                        eprintln!("oo-remote: shell log housekeeping: {e}");
                    }
                });
            }
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

    fn home_net() -> Vec<crate::network::LocalNet> {
        crate::network::networks_from(
            &[(
                "en0".into(),
                "192.168.86.26".parse().unwrap(),
                24,
                Some("192.168.86.1".parse().unwrap()),
            )],
            Some("en0"),
        )
    }

    fn trusted_keys(dir: &Path) -> Vec<String> {
        crate::security::load(dir)
            .unwrap()
            .trusted_networks
            .into_iter()
            .map(|t| t.key)
            .collect()
    }

    #[test]
    fn upgrading_with_paired_devices_trusts_the_current_network_once() {
        let dir = tempfile::tempdir().unwrap();
        migrate_trust(dir.path(), true, &home_net());
        assert_eq!(
            trusted_keys(dir.path()),
            vec!["192.168.86.0/24 via 192.168.86.1"]
        );
        // Later starts (another network, say) add nothing.
        let cafe = crate::network::networks_from(
            &[(
                "en0".into(),
                "10.0.0.7".parse().unwrap(),
                24,
                Some("10.0.0.1".parse().unwrap()),
            )],
            Some("en0"),
        );
        migrate_trust(dir.path(), true, &cafe);
        assert_eq!(trusted_keys(dir.path()).len(), 1);
    }

    #[test]
    fn offline_at_upgrade_tries_again_next_start() {
        let dir = tempfile::tempdir().unwrap();
        migrate_trust(dir.path(), true, &[]);
        assert!(!crate::security::load(dir.path()).unwrap().migrated);
        migrate_trust(dir.path(), true, &home_net());
        assert_eq!(trusted_keys(dir.path()).len(), 1);
    }

    #[test]
    fn a_fresh_setup_trusts_nothing_until_a_device_is_added() {
        let dir = tempfile::tempdir().unwrap();
        migrate_trust(dir.path(), false, &home_net());
        assert!(trusted_keys(dir.path()).is_empty());
        assert!(crate::security::load(dir.path()).unwrap().migrated);
    }

    #[test]
    fn a_corrupt_security_file_trusts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("remote")).unwrap();
        std::fs::write(crate::security::path(dir.path()), "{bad").unwrap();
        migrate_trust(dir.path(), true, &home_net());
        assert!(crate::security::load(dir.path()).is_none());
    }

    #[test]
    fn five_wrong_codes_lock_only_that_address_for_a_while() {
        let (a, b): (IpAddr, IpAddr) = (
            "192.168.1.5".parse().unwrap(),
            "192.168.1.6".parse().unwrap(),
        );
        let t0 = Instant::now();
        let mut l = AddressLocks::default();
        for _ in 0..PAIR_FAILURES_PER_ADDRESS {
            assert!(!l.locked(a, t0));
            l.record(a, false, t0);
        }
        assert!(l.locked(a, t0));
        assert!(!l.locked(b, t0), "another address is unaffected");
        assert!(!l.locked(a, t0 + PAIR_LOCK), "the lock lapses");
    }

    #[test]
    fn a_right_code_clears_the_count() {
        let a: IpAddr = "10.0.0.2".parse().unwrap();
        let t0 = Instant::now();
        let mut l = AddressLocks::default();
        for _ in 0..PAIR_FAILURES_PER_ADDRESS - 1 {
            l.record(a, false, t0);
        }
        l.record(a, true, t0);
        l.record(a, false, t0);
        assert!(!l.locked(a, t0));
    }
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
            branch: None,
            has_conversation: false,
            session: session.into(),
            transcript: None,
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
        let out = compute_agents(&snap, &file, &live, Path::new("/nonexistent"));
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
