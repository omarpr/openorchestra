//! tmux control mode (`oo-session -C attach-session -t =<s> -f ignore-size`):
//! the part of it the phone remote's bridge (`crates/remote/src/tmux_bridge.rs`)
//! reads and writes.
//!
//! Out, on stdout, one line each:
//! - `%session-changed $<id> <name>` once attached;
//! - `%output %<pane> <bytes>` for the session's output, every byte outside
//!   printable ASCII, and `\`, as a `\ooo` octal escape (tmux escapes only
//!   control bytes and `\`; escaping the rest too keeps the stream ASCII);
//! - for each command read, `%begin <time> <n> 1`, the command's own output,
//!   then `%end <time> <n> 1` or `%error <time> <n> 1` (flag 1: this client
//!   sent it);
//! - `%exit` when the session ends or stdin closes.
//!
//! In, on stdin: one command per line, words split on whitespace (the bridge
//! composes every line itself and quotes nothing); an empty line detaches.
//! Only the commands a viewer needs run; any other is answered with `%error`.
//!
//! The client never sizes the session (tmux's `ignore-size`), and it skips
//! the screen the host redraws for a new client: a tmux control client gets
//! only live output and asks for the screen with `capture-pane`.

use std::io::{self, BufRead, Write};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::msg;
use super::pipe::{self, Pipe};
use crate::cmdline::{self, Command};

/// What a control client may run.
const ALLOWED: &[&str] = &[
    "send-keys",
    "resize-window",
    "set-option",
    "display-message",
    "capture-pane",
    "refresh-client",
    "has-session",
    "list-sessions",
    "list-panes",
];

/// Attach to `session` (whose program has `pid`, at `size`) in control mode
/// until it ends or stdin closes; `exec` runs each allowed command. Exits
/// the process when done.
pub fn attach(
    label: &str,
    session: &str,
    pid: u32,
    size: (u16, u16),
    exec: &dyn Fn(&Command) -> i32,
) -> i32 {
    let conn = match Pipe::connect(&pipe::path(label, session), Duration::from_secs(5)) {
        Ok(c) => Arc::new(c),
        Err(_) => {
            eprintln!("can't find session: {session}");
            return 1;
        }
    };
    // The current size, for a host that ignores the flag.
    let mut payload = super::size_payload(size.0, size.1);
    payload.push(msg::ATTACH_IGNORE_SIZE);
    if conn.send(msg::ATTACH, &payload).is_err() {
        eprintln!("can't find session: {session}");
        return 1;
    }
    {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "%session-changed ${pid} {session}");
        let _ = out.flush();
    }

    let pane = format!("%{pid}");
    std::thread::spawn(move || {
        let mut redraw = true;
        loop {
            match conn.recv() {
                Ok(Some((msg::OUTPUT, bytes))) => {
                    if std::mem::take(&mut redraw) {
                        continue;
                    }
                    let mut out = io::stdout().lock();
                    if out
                        .write_all(&output_line(&pane, &bytes))
                        .and_then(|_| out.flush())
                        .is_err()
                    {
                        break;
                    }
                }
                Ok(Some((msg::EXIT, _))) | Ok(None) | Err(_) => break,
                Ok(Some(_)) => {}
            }
        }
        exit();
    });

    let mut number = 0u64;
    for line in io::stdin().lock().split(b'\n') {
        let Ok(line) = line else { break };
        let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(&line)).into_owned();
        if line.trim().is_empty() {
            break;
        }
        number += 1;
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        // Held through the command, so no `%output` lands inside the block
        // (the reader would take it for the command's output). The lock is
        // reentrant: the command's own `println!`s go inside it.
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "%begin {time} {number} 1");
        let words: Vec<String> = line.split_whitespace().map(String::from).collect();
        let ok = match cmdline::parse(&words) {
            Some(cmd) if ALLOWED.contains(&cmd.name.as_str()) => exec(&cmd) == 0,
            Some(cmd) => {
                let _ = writeln!(out, "not allowed in control mode: {}", cmd.name);
                false
            }
            None => false,
        };
        let end = if ok { "%end" } else { "%error" };
        let _ = writeln!(out, "{end} {time} {number} 1");
        if out.flush().is_err() {
            break;
        }
    }
    exit()
}

fn exit() -> ! {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "%exit");
    let _ = out.flush();
    std::process::exit(0)
}

/// `%output <pane> <escaped bytes>\n`.
fn output_line(pane: &str, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pane.len() + bytes.len() + 10);
    out.extend_from_slice(b"%output ");
    out.extend_from_slice(pane.as_bytes());
    out.push(b' ');
    for &b in bytes {
        if (0x20..0x7f).contains(&b) && b != b'\\' {
            out.push(b);
        } else {
            out.extend_from_slice(format!("\\{b:03o}").as_bytes());
        }
    }
    out.push(b'\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_escaped_as_tmux_reads_it() {
        assert_eq!(
            output_line("%7", b"hi\\\r\n\x1b[1m\xc3\xa9"),
            b"%output %7 hi\\134\\015\\012\\033[1m\\303\\251\n".to_vec()
        );
        assert_eq!(output_line("%7", b""), b"%output %7 \n".to_vec());
    }
}
