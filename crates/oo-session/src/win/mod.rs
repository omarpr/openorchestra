//! The Windows implementation: pipes, the ConPTY host, the attach client,
//! the process tree, and the tmux-style command line over them.

pub mod cli;
pub mod client;
pub mod host;
pub mod pipe;
pub mod procs;

/// Message kinds on a session pipe.
pub mod msg {
    // Client → host.
    /// Start streaming: payload `cols: u16, rows: u16` (LE). The host answers
    /// with the screen redrawn, then live output.
    pub const ATTACH: u8 = 1;
    /// Bytes to type into the session.
    pub const INPUT: u8 = 2;
    /// The attached client's size changed: `cols: u16, rows: u16`.
    pub const RESIZE: u8 = 3;
    /// Ask for the session's details; answered with `INFO_REPLY`.
    pub const INFO: u8 = 4;
    /// Set a session option: `name\0value`; answered with `OK`.
    pub const SET_OPTION: u8 = 5;
    /// End the session; answered with `OK`.
    pub const KILL: u8 = 6;
    /// Round trip: answered with `OK` once everything before it is handled.
    pub const PING: u8 = 7;
    /// The screen's text; answered with `OUTPUT`. Payload: one byte, 1 to
    /// keep colours and attributes as escape sequences.
    pub const CAPTURE: u8 = 8;

    // Host → client.
    /// Terminal output.
    pub const OUTPUT: u8 = 101;
    /// The session ended.
    pub const EXIT: u8 = 102;
    /// `key\tvalue` lines (see `host::info`).
    pub const INFO_REPLY: u8 = 103;
    pub const OK: u8 = 104;
}

/// `cols, rows` as a message payload.
pub fn size_payload(cols: u16, rows: u16) -> Vec<u8> {
    let mut p = cols.to_le_bytes().to_vec();
    p.extend_from_slice(&rows.to_le_bytes());
    p
}

/// The inverse of [`size_payload`], clamped to something a ConPTY accepts.
pub fn parse_size(p: &[u8]) -> Option<(u16, u16)> {
    if p.len() < 4 {
        return None;
    }
    let cols = u16::from_le_bytes([p[0], p[1]]).clamp(2, 4000);
    let rows = u16::from_le_bytes([p[2], p[3]]).clamp(2, 2000);
    Some((cols, rows))
}
