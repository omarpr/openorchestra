//! oo-remote — LAN remote control for openOrchestra agent tabs.

mod api;
pub mod assets;
pub mod auth;
pub mod certs;
pub mod conversation;
pub mod keepawake;
pub mod netfilter;
pub mod server;
mod setup;
pub mod store;
mod tls;
pub mod tmux_bridge;
mod ws;

pub use api::flatten_newlines;
pub use conversation::{
    parse_entries, tool_result, tool_summary, Entry, EntryKind, ToolLine, ToolResult, ToolStatus,
    TranscriptKind,
};
pub use keepawake::KeepAwake;
pub use server::{
    snapshot_channel, AgentInfo, RemoteServer, ServerConfig, ServerStatus, Snapshot, StartError,
};
pub use store::{Device, Mark, PendingCode, RemoteFile, Store};
pub use tmux_bridge::Tmux;
