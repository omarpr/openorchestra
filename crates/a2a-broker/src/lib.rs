//! Localhost A2A broker for openOrchestra multi-member tasks.
//!
//! Implements a minimal A2A Protocol v1.0 subset (JSON-RPC over HTTP):
//! Agent Card discovery, SendMessage, GetTask, CancelTask, and SSE streaming
//! subscribe. Coding CLIs are opaque — this crate is the A2A Server facade;
//! the harness bridges into member agent sessions via [`Bridge`].
//!
//! See `docs/superpowers/specs/2026-08-09-multi-context-a2a-orchestration-design.md`.

mod audit;
mod bridge;
mod card;
mod credentials;
mod http;
mod protocol;
mod store;

pub use audit::AuditLog;
pub use bridge::{Bridge, BridgeEvent, FileBridge, InboxDelivery, InboxMessage};
pub use card::{agent_card_for_member, orchestrator_card, AgentCard, AgentSkill};
pub use credentials::BrokerCredentials;
pub use http::{BrokerHandle, BrokerServer};
pub use protocol::{
    Artifact, Message, MessagePart, Role, SendMessageParams, Task, TaskState, TaskStatus,
};
pub use store::TaskStore;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// Tenant id of a task's coordinator agent.
pub const COORDINATOR_TENANT: &str = "@coordinator";

/// Configuration for one openOrchestra task's broker instance.
#[derive(Debug, Clone)]
pub struct BrokerConfig {
    /// openOrchestra task id (becomes A2A `contextId`).
    pub oo_task_id: String,
    pub task_slug: String,
    pub task_name: String,
    /// Tenant that receives un-addressed messages (the coordinator).
    pub coordinator_tenant: String,
    pub members: Vec<MemberEndpoint>,
    /// Bearer token required on JSON-RPC and card endpoints.
    pub api_key: String,
    /// Directory for audit + inbox/outbox (`…/tasks/<slug>/a2a`).
    pub a2a_dir: PathBuf,
    /// Bind address; must be loopback for v1.
    pub bind: SocketAddr,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberEndpoint {
    pub worktree_id: String,
    pub repo: String,
    pub path: PathBuf,
    pub is_coordinator: bool,
}

/// Shared broker state behind the HTTP server.
pub struct BrokerState {
    pub config: BrokerConfig,
    pub tasks: Mutex<TaskStore>,
    pub bridge: Arc<dyn Bridge>,
    pub audit: Mutex<AuditLog>,
    /// Coalesced UI wake-ups when A2A task state changes (optional).
    pub on_change: Option<crossbeam_channel::Sender<()>>,
    /// Live inbox deliveries for harness → agent tmux inject (optional).
    pub on_inbox: Option<crossbeam_channel::Sender<InboxDelivery>>,
}

impl BrokerState {
    pub fn new(config: BrokerConfig, bridge: Arc<dyn Bridge>) -> std::io::Result<Self> {
        Self::with_hooks(config, bridge, None, None)
    }

    pub fn new_with_notify(
        config: BrokerConfig,
        bridge: Arc<dyn Bridge>,
        on_change: Option<crossbeam_channel::Sender<()>>,
    ) -> std::io::Result<Self> {
        Self::with_hooks(config, bridge, on_change, None)
    }

    pub fn with_hooks(
        config: BrokerConfig,
        bridge: Arc<dyn Bridge>,
        on_change: Option<crossbeam_channel::Sender<()>>,
        on_inbox: Option<crossbeam_channel::Sender<InboxDelivery>>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(&config.a2a_dir)?;
        let audit = AuditLog::open(config.a2a_dir.join("audit.jsonl"))?;
        Ok(BrokerState {
            config,
            tasks: Mutex::new(TaskStore::default()),
            bridge,
            audit: Mutex::new(audit),
            on_change,
            on_inbox,
        })
    }

    pub fn notify_change(&self) {
        if let Some(tx) = &self.on_change {
            let _ = tx.try_send(());
        }
    }

    pub fn notify_inbox(&self, delivery: InboxDelivery) {
        if let Some(tx) = &self.on_inbox {
            let _ = tx.send(delivery);
        }
    }

    pub fn member(&self, worktree_id: &str) -> Option<&MemberEndpoint> {
        self.config
            .members
            .iter()
            .find(|m| m.worktree_id == worktree_id)
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.config.bind)
    }
}

/// Generate a random API key for a task broker.
pub fn generate_api_key() -> String {
    format!("oo-{}", uuid::Uuid::new_v4())
}

/// Encode a worktree id for use as a single URL path segment (`/` → `%2F`).
pub fn encode_tenant(worktree_id: &str) -> String {
    worktree_id.replace('/', "%2F")
}

/// Decode a tenant path segment (`%2F` / `%2f` → `/`).
pub fn decode_tenant(segment: &str) -> String {
    segment.replace("%2F", "/").replace("%2f", "/")
}

/// Pane / tmux key for a task member context.
pub fn task_member_pane_key(task_id: &str, worktree_id: &str) -> String {
    format!("task:{task_id}:member:{worktree_id}")
}

/// Pane key of a task's coordinator agent.
pub fn coordinator_pane_key(task_id: &str) -> String {
    task_member_pane_key(task_id, COORDINATOR_TENANT)
}

/// Parse `task:{id}:member:{worktree_id}` → (task_id, worktree_id).
pub fn parse_task_member_pane_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("task:")?;
    let (task_id, member) = rest.split_once(":member:")?;
    if task_id.is_empty() || member.is_empty() {
        return None;
    }
    Some((task_id, member))
}

/// Legacy key `task:{id}` (pre multi-context).
pub fn parse_legacy_task_pane_key(key: &str) -> Option<&str> {
    let id = key.strip_prefix("task:")?;
    if id.is_empty() || id.contains(":member:") {
        return None;
    }
    Some(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_key_round_trip() {
        let k = task_member_pane_key("auth", "jam/auth");
        assert_eq!(k, "task:auth:member:jam/auth");
        assert_eq!(parse_task_member_pane_key(&k), Some(("auth", "jam/auth")));
        assert_eq!(parse_legacy_task_pane_key("task:auth"), Some("auth"));
        assert!(parse_legacy_task_pane_key("task:auth:member:x").is_none());
    }

    #[test]
    fn coordinator_pane_key_round_trips() {
        assert_eq!(
            parse_task_member_pane_key(&coordinator_pane_key("auth")),
            Some(("auth", "@coordinator"))
        );
    }

    #[test]
    fn tenant_encode_round_trip() {
        assert_eq!(encode_tenant("jam/auth"), "jam%2Fauth");
        assert_eq!(decode_tenant("jam%2Fauth"), "jam/auth");
        assert_eq!(decode_tenant("jam%2fauth"), "jam/auth");
        assert_eq!(decode_tenant("solo"), "solo");
    }
}
