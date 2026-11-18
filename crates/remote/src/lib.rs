//! oo-remote — LAN remote control for openOrchestra agent tabs.

pub mod activity;
mod api;
pub mod assets;
pub mod auth;
pub mod certs;
pub mod conversation;
mod conversation_api;
pub mod keepawake;
pub mod netfilter;
pub mod network;
pub mod security;
pub mod server;
mod setup;
pub mod shell;
pub mod store;
mod tls;
pub mod tmux_bridge;
pub mod transcript;
mod voice;
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
pub use transcript::{Tail, Transcripts, Update};
