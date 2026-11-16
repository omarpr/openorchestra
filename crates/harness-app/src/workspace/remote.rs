//! oo-remote glue: the snapshot of marked tabs the phone server publishes,
//! the server's lifecycle (start/stop off the GPUI thread, `OtherBuild`
//! retry), keep-awake, and the `remote.json` watcher.
//!
//! Threading: `RemoteServer::start`/`stop` block (they own a tokio runtime),
//! so they run on the background executor and hand results back through the
//! entity. The GPUI thread only builds snapshots (cheap), reads/writes
//! `remote.json` through the store (one small file), and spawns/kills the
//! `caffeinate` child.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use agent_runtime::AgentState;
use oo_remote::{
    AgentInfo, KeepAwake, RemoteFile, RemoteServer, ServerConfig, ServerStatus, Snapshot,
    StartError, Store, Tmux, snapshot_channel,
};

use super::*;

/// Plain-HTTP setup port (served only while "Add a phone" is open).
const SETUP_PORT: u16 = 7080;
/// Watcher/refresh poll cadence; also the debounce for `remote.json` events.
const TICK: Duration = Duration::from_millis(250);
/// Snapshot + status refresh every this many ticks (1 s).
const REFRESH_TICKS: u32 = 4;
/// While another build holds the port, retry every this many ticks (30 s).
const RETRY_TICKS: u32 = 120;

/// Whether this app instance is serving phones.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RemoteServe {
    Off,
    Serving,
    /// The port is held by the other openOrchestra build (Dev vs release share
    /// `~/.openorchestra`); retried every 30 s so this one takes over when it
    /// quits.
    OtherBuild,
    Error(String),
}

pub(crate) struct RemoteRuntime {
    pub store: Store,
    /// UI cache of `remote.json`, reloaded after our own writes and on every
    /// watcher event. Never written back: every change goes through `store`.
    pub file: RemoteFile,
    pub serve: RemoteServe,
    pub status: Option<ServerStatus>,
    state_dir: PathBuf,
    server: Option<RemoteServer>,
    snapshot_tx: tokio::sync::watch::Sender<Snapshot>,
    keep_awake: Option<KeepAwake>,
    /// The store's file watcher (type-erased: two `notify` versions are in
    /// the graph) and its change events, taken by the poll loop.
    _watcher: Option<Box<dyn std::any::Any>>,
    changes: Option<crossbeam_channel::Receiver<()>>,
    /// A start/stop is running on the background executor.
    starting: bool,
    stopping: bool,
}

impl RemoteRuntime {
    pub(crate) fn new(state_dir: &Path) -> RemoteRuntime {
        let store = Store::new(state_dir);
        let file = store.load();
        let (watcher, changes) = match store.watch() {
            Ok((w, rx)) => (Some(Box::new(w) as Box<dyn std::any::Any>), Some(rx)),
            Err(e) => {
                log::warn!("remote: cannot watch {}: {e}", store.path().display());
                (None, None)
            }
        };
        let (snapshot_tx, _) = snapshot_channel();
        RemoteRuntime {
            store,
            file,
            serve: RemoteServe::Off,
            status: None,
            state_dir: state_dir.to_path_buf(),
            server: None,
            snapshot_tx,
            keep_awake: None,
            _watcher: watcher,
            changes,
            starting: false,
            stopping: false,
        }
    }

    fn reload(&mut self) {
        self.file = self.store.load();
    }

    /// Hold `caffeinate` iff serving with keep-awake on.
    fn sync_keep_awake(&mut self) {
        let want = self.serve == RemoteServe::Serving && self.file.keep_awake;
        if want && self.keep_awake.is_none() {
            match KeepAwake::start() {
                Ok(k) => self.keep_awake = Some(k),
                Err(e) => log::warn!("remote: keep-awake failed: {e}"),
            }
        } else if !want {
            self.keep_awake = None;
        }
    }
}

/// One worktree's terminal tabs, as the snapshot builder sees them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MarkSource {
    /// Pane key (`main:<repo>`, a worktree id, a task member / coordinator key).
    pub key: String,
    /// The `agent_status` key for this worktree.
    pub root: PathBuf,
    pub project: String,
    pub worktree: String,
    /// `(tab id, title, agent kind)`, terminal tabs only.
    pub tabs: Vec<(u64, String, Option<String>)>,
}

/// The marked tabs present in `sources`, with their worktree's status. Pure:
/// a mark whose tab isn't live or persisted here is left out, and the tmux
/// session is derived from the mark's key and tab, never sent by a phone.
pub(crate) fn build_snapshot(
    file: &RemoteFile,
    sources: &[MarkSource],
    status: &HashMap<PathBuf, AgentState>,
    since: &HashMap<PathBuf, (SystemTime, Option<String>)>,
) -> Snapshot {
    let mut agents = Vec::new();
    for src in sources {
        let state = status.get(&src.root).copied();
        let (status_since, tool) = match (state, since.get(&src.root)) {
            (Some(s), Some((at, tool))) if s != AgentState::Unknown => {
                (Some(rfc3339(*at)), tool.clone())
            }
            _ => (None, None),
        };
        let status = state.unwrap_or(AgentState::Unknown).to_string();
        for (tab_id, title, kind) in &src.tabs {
            let Some(mark) = file.mark_for(&src.key, *tab_id) else {
                continue;
            };
            agents.push(AgentInfo {
                id: mark.id.clone(),
                title: title.clone(),
                agent_kind: kind.clone(),
                project: src.project.clone(),
                worktree: src.worktree.clone(),
                status: status.clone(),
                status_since: status_since.clone(),
                tool: tool.clone(),
                session: crate::tmux::session_name(&src.key, *tab_id),
            });
        }
    }
    Snapshot { agents }
}

fn rfc3339(t: SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into())
}

impl HarnessWorkspace {
    /// Start the watcher/refresh loop and, when `remote.json` says enabled,
    /// the server. Called once from `new`.
    pub(crate) fn remote_init(&mut self, cx: &mut Context<Self>) {
        let changes = self.remote.changes.take();
        cx.spawn(async move |view, cx| {
            let mut ticks: u32 = 0;
            loop {
                cx.background_executor().timer(TICK).await;
                ticks = ticks.wrapping_add(1);
                let mut changed = false;
                if let Some(rx) = &changes {
                    while rx.try_recv().is_ok() {
                        changed = true;
                    }
                }
                let refresh = ticks.is_multiple_of(REFRESH_TICKS);
                let retry = ticks.is_multiple_of(RETRY_TICKS);
                if !(changed || refresh || retry) {
                    continue;
                }
                let alive = view.update(cx, |ws, cx| {
                    if changed {
                        ws.remote.reload();
                        ws.remote_reconcile(cx);
                    }
                    if retry {
                        ws.remote_retry_other_build(cx);
                    }
                    if refresh || changed {
                        ws.remote_refresh(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        self.remote_refresh(cx);
        self.remote_reconcile(cx);
    }

    /// Every worktree that can hold tabs (same enumeration as
    /// `apply_resource`), with its terminal tabs. Live pane sets come from
    /// `self.panes`; a worktree whose panes are evicted or not yet restored
    /// falls back to its persisted records, since its tmux sessions live on.
    pub(crate) fn remote_sources(&self) -> Vec<MarkSource> {
        let mut out = Vec::new();
        for repo in &self.data.repos {
            out.push(self.remote_source(
                format!("main:{}", repo.name),
                repo.repo_root.clone(),
                repo.name.clone(),
                "main".to_string(),
            ));
            for w in &repo.worktrees {
                let key = w
                    .task_id
                    .as_ref()
                    .map(|tid| a2a_broker::task_member_pane_key(tid, &w.id))
                    .unwrap_or_else(|| w.id.clone());
                out.push(self.remote_source(
                    key,
                    w.path.clone(),
                    repo.name.clone(),
                    w.name.clone(),
                ));
            }
        }
        let tasks_root = self.data_dir.join("tasks");
        for task in self
            .task_store
            .active()
            .filter(|t| super::coordinator::has_coordinator(t))
        {
            out.push(self.remote_source(
                a2a_broker::coordinator_pane_key(&task.id),
                worktrees::task_coordinator_dir(&tasks_root, &task.slug),
                task.name.clone(),
                "coordinator".to_string(),
            ));
        }
        out
    }

    fn remote_source(
        &self,
        key: String,
        root: PathBuf,
        project: String,
        worktree: String,
    ) -> MarkSource {
        use crate::panes_state::TabRecordKind as K;
        let tabs = if let Some(pane) = self.panes.get(&key) {
            pane.tabs
                .iter()
                .filter(|t| matches!(t.content, TabContent::Terminal(_)))
                .map(|t| {
                    (
                        t.id,
                        t.title.clone(),
                        t.agent.as_ref().map(|a| a.as_str().to_string()),
                    )
                })
                .collect()
        } else if let Some(saved) = self.panes_state.worktrees.get(&key) {
            saved
                .tabs
                .iter()
                .filter_map(|t| match &t.kind {
                    K::Shell => Some((t.id, t.title.clone(), None)),
                    K::Agent(a) => Some((t.id, t.title.clone(), Some(a.as_str().to_string()))),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        MarkSource {
            key,
            root,
            project,
            worktree,
            tabs,
        }
    }

    /// Rebuild the snapshot and publish it if it changed.
    pub(crate) fn remote_refresh(&mut self, cx: &mut Context<Self>) {
        self.remote_publish();
        if let Some(server) = &self.remote.server {
            let status = server.status();
            if self.remote.status.as_ref() != Some(&status) {
                self.remote.status = Some(status);
                cx.notify();
            }
        }
    }

    /// `remote_refresh` without a context (for call sites that have none).
    fn remote_publish(&mut self) {
        let snap = build_snapshot(
            &self.remote.file,
            &self.remote_sources(),
            &self.agent_status,
            &self.agent_since,
        );
        self.remote.snapshot_tx.send_if_modified(|cur| {
            if *cur == snap {
                return false;
            }
            *cur = snap;
            true
        });
    }

    #[allow(dead_code)] // called by the Remote settings / tab menu UI (Tasks 10–11)
    pub(crate) fn remote_set_enabled(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(e) = self.remote.store.update(|f| f.enabled = on) {
            log::warn!("remote: saving enabled={on} failed: {e}");
            self.set_note(format!("remote: could not save setting: {e}"));
        }
        self.remote.reload();
        // An explicit enable retries a failed start.
        if on && matches!(self.remote.serve, RemoteServe::Error(_)) {
            self.remote.serve = RemoteServe::Off;
        }
        self.remote_reconcile(cx);
    }

    #[allow(dead_code)] // called by the Remote settings / tab menu UI (Tasks 10–11)
    pub(crate) fn remote_set_keep_awake(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Err(e) = self.remote.store.update(|f| f.keep_awake = on) {
            log::warn!("remote: saving keep_awake={on} failed: {e}");
            self.set_note(format!("remote: could not save setting: {e}"));
        }
        self.remote.reload();
        self.remote.sync_keep_awake();
        cx.notify();
    }

    #[allow(dead_code)] // called by the Remote settings / tab menu UI (Tasks 10–11)
    pub(crate) fn remote_toggle_mark(&mut self, key: &str, tab_id: u64, cx: &mut Context<Self>) {
        // Decide on the file's current contents, not the cache.
        let marked = self.remote.store.load().mark_for(key, tab_id).is_some();
        let result = if marked {
            self.remote.store.unmark(key, Some(tab_id))
        } else {
            self.remote.store.mark(key, tab_id).map(|_| ())
        };
        if let Err(e) = result {
            log::warn!("remote: toggling mark failed: {e}");
            self.set_note(format!("remote: could not save mark: {e}"));
        }
        self.remote.reload();
        self.remote_refresh(cx);
        cx.notify();
    }

    /// Cheap (cached file only): safe to call from render.
    #[allow(dead_code)] // called by the Remote settings / tab menu UI (Tasks 10–11)
    pub(crate) fn remote_is_marked(&self, key: &str, tab_id: u64) -> bool {
        self.remote.file.mark_for(key, tab_id).is_some()
    }

    /// Open or close the plain-HTTP setup port ("Add a phone" panel). A
    /// no-op unless this instance is serving. Binding is a quick syscall.
    #[allow(dead_code)] // called by the Remote settings / tab menu UI (Tasks 10–11)
    pub(crate) fn remote_set_setup_open(&mut self, open: bool) {
        if let Some(server) = &self.remote.server {
            server.set_setup_open(open);
        }
    }

    /// Drop marks for a closed tab (`Some`) or a removed worktree (`None`).
    /// Writes `remote.json` only when something there actually matches.
    pub(crate) fn remote_unmark(&mut self, key: &str, tab_id: Option<u64>) {
        let current = self.remote.store.load();
        let hit = current
            .marked
            .iter()
            .any(|m| m.worktree_key == key && tab_id.is_none_or(|t| m.tab_id == t));
        if !hit {
            return;
        }
        if let Err(e) = self.remote.store.unmark(key, tab_id) {
            log::warn!("remote: unmarking {key} failed: {e}");
        }
        self.remote.reload();
        self.remote_publish();
    }

    /// Bring the server in line with `file.enabled`. Idempotent; a start or
    /// stop in flight finishes first and calls back in here.
    fn remote_reconcile(&mut self, cx: &mut Context<Self>) {
        if self.remote.file.enabled {
            if self.remote.server.is_none()
                && !self.remote.starting
                && !self.remote.stopping
                && self.remote.serve == RemoteServe::Off
            {
                self.remote_start(cx);
            }
        } else {
            self.remote_stop(cx);
            self.remote.serve = RemoteServe::Off;
            self.remote.status = None;
        }
        self.remote.sync_keep_awake();
        cx.notify();
    }

    /// The 30 s retry while the other build holds the port.
    fn remote_retry_other_build(&mut self, cx: &mut Context<Self>) {
        if self.remote.file.enabled
            && self.remote.serve == RemoteServe::OtherBuild
            && !self.remote.starting
            && !self.remote.stopping
        {
            self.remote_start(cx);
        }
    }

    fn remote_start(&mut self, cx: &mut Context<Self>) {
        self.remote.starting = true;
        // The server reads the channel's current value on start.
        self.remote_publish();
        let cfg = ServerConfig {
            state_dir: self.remote.state_dir.clone(),
            port: self.remote.file.port,
            setup_port: SETUP_PORT,
            tmux: Tmux::default(),
            bind_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            hosts_override: None,
        };
        let rx = self.remote.snapshot_tx.subscribe();
        cx.spawn(async move |view, cx| {
            let result = cx
                .background_spawn(async move { RemoteServer::start(cfg, rx) })
                .await;
            let mut result = Some(result);
            let _ = view.update(cx, |ws, cx| {
                if let Some(r) = result.take() {
                    ws.remote_started(r, cx);
                }
            });
            // The workspace is gone: stop off-thread rather than blocking
            // here in `Drop`.
            if let Some(Ok(server)) = result {
                cx.background_spawn(async move { server.stop() }).detach();
            }
        })
        .detach();
    }

    fn remote_started(&mut self, result: Result<RemoteServer, StartError>, cx: &mut Context<Self>) {
        self.remote.starting = false;
        let enabled = self.remote.file.enabled;
        match result {
            Ok(server) if enabled => {
                self.remote.status = Some(server.status());
                self.remote.server = Some(server);
                self.remote.serve = RemoteServe::Serving;
            }
            // Disabled while starting: let it go again.
            Ok(server) => {
                self.remote.server = Some(server);
                self.remote_stop(cx);
                self.remote.serve = RemoteServe::Off;
            }
            Err(StartError::PortInUse(port)) => {
                log::info!("remote: port {port} in use (other build?); retrying every 30 s");
                self.remote.serve = if enabled {
                    RemoteServe::OtherBuild
                } else {
                    RemoteServe::Off
                };
            }
            Err(StartError::Other(e)) => {
                log::warn!("remote: server failed to start: {e}");
                self.remote.serve = if enabled {
                    RemoteServe::Error(e)
                } else {
                    RemoteServe::Off
                };
            }
        }
        self.remote.sync_keep_awake();
        cx.notify();
    }

    /// Stop the server (if running) on the background executor: `stop`
    /// waits for sockets to flush and close.
    fn remote_stop(&mut self, cx: &mut Context<Self>) {
        let Some(server) = self.remote.server.take() else {
            return;
        };
        self.remote.stopping = true;
        self.remote.status = None;
        cx.spawn(async move |view, cx| {
            cx.background_spawn(async move { server.stop() }).await;
            let _ = view.update(cx, |ws, cx| {
                ws.remote.stopping = false;
                ws.remote_reconcile(cx);
            });
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use oo_remote::Mark;
    use std::time::{Duration, UNIX_EPOCH};

    fn mark(key: &str, tab: u64, id: &str) -> Mark {
        Mark {
            worktree_key: key.into(),
            tab_id: tab,
            id: id.into(),
        }
    }

    fn source(key: &str, root: &str, tabs: &[(u64, &str, Option<&str>)]) -> MarkSource {
        MarkSource {
            key: key.into(),
            root: PathBuf::from(root),
            project: "proj".into(),
            worktree: "wt".into(),
            tabs: tabs
                .iter()
                .map(|(id, t, k)| (*id, t.to_string(), k.map(str::to_string)))
                .collect(),
        }
    }

    fn file(marks: Vec<Mark>) -> RemoteFile {
        RemoteFile {
            marked: marks,
            ..RemoteFile::default()
        }
    }

    #[test]
    fn only_marked_tabs_with_session_status_since_and_tool() {
        let f = file(vec![mark("main:proj", 7, "r_abc")]);
        let sources = [source(
            "main:proj",
            "/r/proj",
            &[(7, "claude", Some("claude")), (8, "❯_ shell", None)],
        )];
        let status = HashMap::from([(PathBuf::from("/r/proj"), AgentState::Waiting)]);
        let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let since = HashMap::from([(PathBuf::from("/r/proj"), (at, Some("Bash".to_string())))]);

        let snap = build_snapshot(&f, &sources, &status, &since);

        assert_eq!(snap.agents.len(), 1);
        let a = &snap.agents[0];
        assert_eq!(a.id, "r_abc");
        assert_eq!(a.title, "claude");
        assert_eq!(a.agent_kind.as_deref(), Some("claude"));
        assert_eq!(a.project, "proj");
        assert_eq!(a.worktree, "wt");
        assert_eq!(a.session, crate::tmux::session_name("main:proj", 7));
        assert_eq!(a.status, "waiting");
        assert_eq!(a.status_since.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(a.tool.as_deref(), Some("Bash"));
    }

    #[test]
    fn a_mark_whose_tab_is_not_in_sources_is_omitted() {
        let f = file(vec![
            mark("main:proj", 99, "r_gone"),
            mark("wt-x", 1, "r_other"),
        ]);
        let sources = [source("main:proj", "/r/proj", &[(7, "shell", None)])];
        let snap = build_snapshot(&f, &sources, &HashMap::new(), &HashMap::new());
        assert!(snap.agents.is_empty(), "{:?}", snap.agents);
    }

    #[test]
    fn two_marked_tabs_in_one_worktree_share_its_status() {
        let f = file(vec![mark("wt-1", 3, "r_a"), mark("wt-1", 4, "r_b")]);
        let sources = [source(
            "wt-1",
            "/w/1",
            &[(3, "claude", Some("claude")), (4, "shell", None)],
        )];
        let status = HashMap::from([(PathBuf::from("/w/1"), AgentState::Running)]);
        let snap = build_snapshot(&f, &sources, &status, &HashMap::new());
        let ids: Vec<_> = snap.agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["r_a", "r_b"]);
        assert!(snap.agents.iter().all(|a| a.status == "running"));
        assert_eq!(snap.agents[1].session, crate::tmux::session_name("wt-1", 4));
    }

    #[test]
    fn unknown_and_missing_status_read_unknown() {
        let f = file(vec![mark("a", 1, "r_a"), mark("b", 2, "r_b")]);
        let sources = [
            source("a", "/a", &[(1, "t", None)]),
            source("b", "/b", &[(2, "t", None)]),
        ];
        let status = HashMap::from([(PathBuf::from("/a"), AgentState::Unknown)]);
        let snap = build_snapshot(&f, &sources, &status, &HashMap::new());
        assert_eq!(snap.agents.len(), 2);
        assert!(snap.agents.iter().all(|a| a.status == "unknown"));
        assert!(
            snap.agents
                .iter()
                .all(|a| a.status_since.is_none() && a.tool.is_none())
        );
    }
}
