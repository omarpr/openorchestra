//! A2A broker lifecycle for multi-member openOrchestra tasks.

use super::*;

use a2a_broker::{
    BrokerConfig, BrokerCredentials, BrokerServer, BrokerState, FileBridge, InboxDelivery,
    InboxMessage, MemberEndpoint,
};
use std::net::SocketAddr;
use std::sync::Arc;

impl HarnessWorkspace {
    /// Start (or reuse) the localhost A2A broker for a multi-member task.
    pub(crate) fn ensure_task_broker(&mut self, task_id: &str) {
        if self.task_brokers.contains_key(task_id) {
            return;
        }
        let Some(task) = self.task_store.get(task_id).cloned() else {
            return;
        };
        if task.members.len() < 2 {
            return;
        }
        let Some(home) = task.home_member() else {
            return;
        };
        let a2a_dir = self.data_dir.join("tasks").join(&task.slug).join("a2a");
        let mut creds = match BrokerCredentials::load_or_create(&a2a_dir) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("a2a credentials load failed: {e}");
                return;
            }
        };
        let api_key = creds.api_key.clone();
        let members: Vec<MemberEndpoint> = task
            .members
            .iter()
            .map(|m| MemberEndpoint {
                worktree_id: m.worktree_id.clone(),
                repo: m.repo.clone(),
                path: m.path.clone(),
                is_home: m.repo == task.home_repo,
            })
            .collect();

        let bridge = match FileBridge::new(&a2a_dir) {
            Ok(b) => Arc::new(b),
            Err(e) => {
                log::warn!("a2a bridge init failed: {e}");
                return;
            }
        };

        let preferred_port = creds.port.unwrap_or(0);
        let handle = match start_broker_with_port_fallback(
            &task,
            home,
            &members,
            &api_key,
            &a2a_dir,
            preferred_port,
            bridge,
            self.a2a_change_tx.clone(),
            self.a2a_inbox_tx.clone(),
        ) {
            Ok(h) => h,
            Err(e) => {
                log::warn!("a2a broker start failed: {e}");
                return;
            }
        };

        let base_url = handle.base_url();
        creds.port = Some(handle.addr.port());
        if let Err(e) = creds.save(&a2a_dir) {
            log::warn!("a2a credentials save failed: {e}");
        }

        let tasks_root = self.data_dir.join("tasks");
        if let Some(task) = self.task_store.get(task_id) {
            let _ = worktrees::write_task_agent_context_with_a2a(
                task,
                &tasks_root,
                Some(worktrees::A2aContextInfo {
                    base_url: base_url.clone(),
                    api_key: api_key.clone(),
                }),
            );
        }
        write_a2a_helpers(&a2a_dir, &base_url, &api_key);
        self.task_brokers.insert(
            task_id.to_string(),
            TaskBrokerInfo {
                base_url,
                api_key,
                handle,
            },
        );
        log::info!("a2a broker started for task {task_id}");
    }

    pub(crate) fn shutdown_task_broker(&mut self, task_id: &str) {
        if let Some(info) = self.task_brokers.remove(task_id) {
            info.handle.shutdown();
            log::info!("a2a broker stopped for task {task_id}");
        }
    }

    /// Snapshot of A2A task states for the selected openOrchestra task (UI).
    pub(crate) fn a2a_delegation_summary(&self) -> Vec<(String, String, String)> {
        let Some(r) = self.resolve() else {
            return Vec::new();
        };
        let Some(tid) = &r.task_id else {
            return Vec::new();
        };
        let Some(info) = self.task_brokers.get(tid) else {
            return Vec::new();
        };
        let Ok(tasks) = info.handle.state.tasks.lock() else {
            return Vec::new();
        };
        tasks
            .list_for_context(tid)
            .into_iter()
            .map(|t| {
                let state = serde_json::to_value(t.status.state)
                    .ok()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown".into());
                (t.id.clone(), t.tenant.clone(), state)
            })
            .collect()
    }

    /// Deliver an A2A inbox message into the specialist's agent tmux session,
    /// or queue it until an Agent tab is available.
    pub(crate) fn deliver_a2a_inbox(&mut self, delivery: InboxDelivery) {
        let pane_key =
            a2a_broker::task_member_pane_key(&delivery.oo_task_id, &delivery.tenant);
        if self.try_inject_a2a_message(&pane_key, &delivery.message) {
            return;
        }
        self.a2a_pending_inject
            .entry(pane_key)
            .or_default()
            .push(delivery.message);
        log::info!(
            "a2a inbox queued for {} (no live agent session yet)",
            delivery.tenant
        );
    }

    /// Flush any queued A2A inbox lines for a member pane (e.g. after Open Agent).
    pub(crate) fn flush_a2a_inbox_for_pane(&mut self, pane_key: &str) {
        let Some(pending) = self.a2a_pending_inject.remove(pane_key) else {
            return;
        };
        let mut still = Vec::new();
        for msg in pending {
            if !self.try_inject_a2a_message(pane_key, &msg) {
                still.push(msg);
            }
        }
        if !still.is_empty() {
            self.a2a_pending_inject.insert(pane_key.to_string(), still);
        }
    }

    fn try_inject_a2a_message(&self, pane_key: &str, msg: &InboxMessage) -> bool {
        if !self.has_tmux {
            return false;
        }
        let Some(session) = self.agent_tmux_session_for(pane_key) else {
            return false;
        };
        let a2a_dir = match a2a_broker::parse_task_member_pane_key(pane_key) {
            Some((tid, _)) => self
                .task_store
                .get(tid)
                .map(|t| self.data_dir.join("tasks").join(&t.slug).join("a2a")),
            None => None,
        };
        let line = format_a2a_inject(msg, a2a_dir.as_deref());
        crate::tmux::send_keys(&session, &line);
        log::info!("a2a inbox injected into session {session}");
        true
    }

    /// Prefer a live Agent-tab tmux session for this pane key.
    fn agent_tmux_session_for(&self, pane_key: &str) -> Option<String> {
        let live = crate::tmux::live_sessions();
        let is_live = |name: &str| live.iter().any(|s| s == name);

        if let Some(pane) = self.panes.get(pane_key) {
            if let Some(tab) = pane.active_tab()
                && tab.agent.is_some() {
                    let name = crate::tmux::session_name(pane_key, tab.id);
                    if is_live(&name) {
                        return Some(name);
                    }
                }
            for tab in pane.tabs.iter().rev() {
                if tab.agent.is_some() {
                    let name = crate::tmux::session_name(pane_key, tab.id);
                    if is_live(&name) {
                        return Some(name);
                    }
                }
            }
        }

        if let Some(wp) = self.panes_state.worktrees.get(pane_key) {
            for t in wp.tabs.iter().rev() {
                if matches!(t.kind, crate::panes_state::TabRecordKind::Agent(_)) {
                    let name = crate::tmux::session_name(pane_key, t.id);
                    if is_live(&name) {
                        return Some(name);
                    }
                }
            }
        }
        None
    }
}

// Every argument is a distinct input to the broker config; bundling them into
// a one-off struct would only move the list.
#[allow(clippy::too_many_arguments)]
fn start_broker_with_port_fallback(
    task: &worktrees::Task,
    home: &worktrees::TaskMember,
    members: &[MemberEndpoint],
    api_key: &str,
    a2a_dir: &std::path::Path,
    preferred_port: u16,
    bridge: Arc<FileBridge>,
    on_change: crossbeam_channel::Sender<()>,
    on_inbox: crossbeam_channel::Sender<InboxDelivery>,
) -> std::io::Result<a2a_broker::BrokerHandle> {
    let try_port = |port: u16| -> std::io::Result<a2a_broker::BrokerHandle> {
        let bind: SocketAddr = ([127, 0, 0, 1], port).into();
        let config = BrokerConfig {
            oo_task_id: task.id.clone(),
            task_slug: task.slug.clone(),
            task_name: task.name.clone(),
            home_worktree_id: home.worktree_id.clone(),
            home_repo: home.repo.clone(),
            members: members.to_vec(),
            api_key: api_key.to_string(),
            a2a_dir: a2a_dir.to_path_buf(),
            bind,
        };
        let state = BrokerState::with_hooks(
            config,
            bridge.clone(),
            Some(on_change.clone()),
            Some(on_inbox.clone()),
        )?;
        BrokerServer::start(state)
    };

    if preferred_port != 0 {
        match try_port(preferred_port) {
            Ok(h) => return Ok(h),
            Err(e) => {
                log::warn!(
                    "a2a bind {}:{} failed ({e}); falling back to ephemeral port",
                    "127.0.0.1",
                    preferred_port
                );
            }
        }
    }
    try_port(0)
}

fn format_a2a_inject(msg: &InboxMessage, a2a_dir: Option<&std::path::Path>) -> String {
    // Flatten newlines so tmux send-keys -l + Enter does not submit mid-message.
    let text = msg
        .text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect::<String>();
    let complete_hint = a2a_dir
        .map(|d| {
            format!(
                " — when done: {} {} \"summary\"",
                d.join("complete.sh").display(),
                msg.a2a_task_id
            )
        })
        .unwrap_or_default();
    format!(
        "[openOrchestra A2A] task_id={} — {}{}",
        msg.a2a_task_id, text, complete_hint
    )
}

fn write_a2a_helpers(a2a_dir: &std::path::Path, base_url: &str, api_key: &str) {
    let _ = std::fs::create_dir_all(a2a_dir);
    // Plain sh + curl. `--data-urlencode` does all the escaping, so a message
    // with quotes or newlines arrives intact without python or jq to build
    // JSON (the broker's form endpoints exist for exactly this).
    let delegate = format!(
        r#"#!/bin/sh
# Usage: ./delegate.sh <worktree_id> <message>
set -e
TENANT="${{1:?tenant}}"
MSG="${{2:?message}}"
ENC=$(printf '%s' "$TENANT" | sed 's|/|%2F|g')
curl -sS -X POST "{base}/t/${{ENC}}/send" \
  -H "Authorization: Bearer {key}" \
  --data-urlencode "text=$MSG"
echo
"#,
        base = base_url,
        key = api_key,
    );
    write_script(&a2a_dir.join("delegate.sh"), &delegate);

    let complete = format!(
        r#"#!/bin/sh
# Usage: ./complete.sh <a2a_task_id> [summary]
set -e
ID="${{1:?a2a_task_id}}"
SUMMARY="${{2:-done}}"
curl -sS -X POST "{base}/v1/complete" \
  -H "Authorization: Bearer {key}" \
  --data-urlencode "a2a_task_id=$ID" \
  --data-urlencode "summary=$SUMMARY"
echo
"#,
        base = base_url,
        key = api_key,
    );
    write_script(&a2a_dir.join("complete.sh"), &complete);

    let status = format!(
        r#"#!/bin/sh
# Usage: ./status.sh <a2a_task_id>
set -e
ID="${{1:?a2a_task_id}}"
curl -sS -X POST "{base}/v1/status" \
  -H "Authorization: Bearer {key}" \
  --data-urlencode "id=$ID"
echo
"#,
        base = base_url,
        key = api_key,
    );
    write_script(&a2a_dir.join("status.sh"), &status);

    let tools = serde_json::json!({
        "tools": [
            {
                "name": "delegate_to_agent",
                "description": "Send an A2A message to a sibling specialist (worktree_id tenant).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "worktree_id": { "type": "string" },
                        "message": { "type": "string" }
                    },
                    "required": ["worktree_id", "message"]
                }
            },
            {
                "name": "get_delegation_status",
                "description": "Get A2A task status by id.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "a2a_task_id": { "type": "string" }
                    },
                    "required": ["a2a_task_id"]
                }
            },
            {
                "name": "complete_delegation",
                "description": "Mark an inbound A2A task completed (specialists).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "a2a_task_id": { "type": "string" },
                        "summary": { "type": "string" }
                    },
                    "required": ["a2a_task_id"]
                }
            }
        ],
        "base_url": base_url,
        "note": "Invoke via shell helpers in this directory (delegate.sh / status.sh / complete.sh) or HTTP JSON-RPC."
    });
    let _ = std::fs::write(
        a2a_dir.join("mcp-tools.json"),
        serde_json::to_string_pretty(&tools).unwrap_or_default(),
    );
}

/// Write a helper script that carries the broker key: owner-only, since the
/// key is all it takes to type into this task's agents (0700, not 0755).
fn write_script(path: &std::path::Path, body: &str) {
    if std::fs::write(path, body).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
        }
    }
}
