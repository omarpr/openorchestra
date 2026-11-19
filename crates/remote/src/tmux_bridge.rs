//! tmux control-mode bridge: the terminal security boundary for oo-remote.
//!
//! A phone is never attached as an interactive tmux client (its prefix key
//! could switch sessions or run commands). Instead the server owns one
//! control-mode client per session (`tmux -C attach-session -t =<s> -f
//! ignore-size`), forwards only the marked pane's `%output`, and turns phone
//! keystrokes into `send-keys -t <pane> -H <hex…>` lines it composes itself.
//!
//! Injection rules for anything written to the control client's stdin: the
//! only interpolated values are the pane id (validated `%[0-9]+`), hex tokens
//! derived from bytes, and integers. Session names are only ever passed as
//! argv to separate `tmux` processes, as exact-match targets (`=<name>`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::store::PendingScreenshot;

/// Input bytes per `send-keys -H` line.
const KEYS_PER_LINE: usize = 256;
/// Broadcast buffer (events) per hub; slow subscribers see `Lagged`.
const EVENT_CAPACITY: usize = 1024;
/// Pause between a quick reply's text and its Enter (separate pty writes).
const ENTER_DELAY: Duration = Duration::from_millis(50);
/// How long to wait for tmux to acknowledge a control command.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Reason sent with [`HubEvent::Closed`] when the session/control client ends.
const STOPPED: &str = "stopped";

/// Which tmux server to talk to. `Tmux::default()` is the user's default
/// server; tests set `socket` to a private `-L` name.
#[derive(Clone, Default, Debug)]
pub struct Tmux {
    pub socket: Option<String>,
}

impl Tmux {
    /// A `tmux` command, with `-L <socket>` when set.
    pub fn command(&self) -> Command {
        let mut c = Command::new("tmux");
        if let Some(s) = &self.socket {
            c.arg("-L").arg(s);
        }
        c
    }

    /// A blocking `std` `tmux` command, with `-L <socket>` when set.
    fn std_command(&self) -> std::process::Command {
        let mut c = std::process::Command::new("tmux");
        if let Some(s) = &self.socket {
            c.arg("-L").arg(s);
        }
        c
    }

    /// Run a one-shot tmux command (argv only); stdout on success.
    async fn output(&self, args: &[&str]) -> io::Result<Vec<u8>> {
        let out = self
            .command()
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await?;
        if !out.status.success() {
            return Err(io::Error::other(format!(
                "tmux {}: {}",
                args.first().copied().unwrap_or(""),
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(out.stdout)
    }
}

/// Names of all sessions on the server (empty if no server is running).
pub async fn live_sessions(t: &Tmux) -> HashSet<String> {
    try_live_sessions(t).await.unwrap_or_default()
}

/// [`live_sessions`], telling a failed listing apart from an empty one (for
/// anything that deletes what isn't listed).
pub async fn try_live_sessions(t: &Tmux) -> io::Result<HashSet<String>> {
    let out = t
        .output(&["list-sessions", "-F", "#{session_name}"])
        .await?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// One notification line from a control-mode client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    Output {
        pane: String,
        data: Vec<u8>,
    },
    Exit,
    /// `%session-changed $<id> <name>`: the client is now attached to `id`.
    SessionChanged {
        id: String,
        name: String,
    },
    Other,
}

/// Parse one control-mode line (without the trailing newline).
pub fn parse_line(line: &str) -> ControlEvent {
    parse_line_bytes(line.as_bytes())
}

/// Byte-level parser: `%output` payloads may carry raw non-UTF-8 bytes
/// (tmux only escapes bytes < 0x20 and `\`).
fn parse_line_bytes(line: &[u8]) -> ControlEvent {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    if line == b"%exit" || line.starts_with(b"%exit ") {
        return ControlEvent::Exit;
    }
    if let Some(rest) = line.strip_prefix(b"%session-changed ") {
        // The name is the rest of the line (it may contain spaces).
        let rest = String::from_utf8_lossy(rest);
        if let Some((id, name)) = rest.split_once(' ') {
            if id.starts_with('$') {
                return ControlEvent::SessionChanged {
                    id: id.to_string(),
                    name: name.to_string(),
                };
            }
        }
        return ControlEvent::Other;
    }
    if let Some(rest) = line.strip_prefix(b"%output ") {
        let (pane, escaped) = match rest.iter().position(|&b| b == b' ') {
            Some(i) => (&rest[..i], &rest[i + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        if let Ok(pane) = std::str::from_utf8(pane) {
            if valid_pane_id(pane) {
                return ControlEvent::Output {
                    pane: pane.to_string(),
                    data: decode_bytes(escaped),
                };
            }
        }
    }
    ControlEvent::Other
}

/// Undo tmux's `%output` escaping: `\ooo` octal and `\\`.
pub fn decode_output(escaped: &str) -> Vec<u8> {
    decode_bytes(escaped.as_bytes())
}

fn decode_bytes(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let b = s[i];
        if b == b'\\' {
            let oct = |c: u8| (b'0'..=b'7').contains(&c);
            if i + 3 < s.len()
                && oct(s[i + 1])
                && oct(s[i + 2])
                && oct(s[i + 3])
                && s[i + 1] <= b'3'
            {
                out.push((s[i + 1] - b'0') * 64 + (s[i + 2] - b'0') * 8 + (s[i + 3] - b'0'));
                i += 4;
                continue;
            }
            if s.get(i + 1) == Some(&b'\\') {
                out.push(b'\\');
                i += 2;
                continue;
            }
        }
        out.push(b);
        i += 1;
    }
    out
}

/// `%[0-9]+`
fn valid_pane_id(pane: &str) -> bool {
    pane.strip_prefix('%')
        .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
}

/// Session names are only ever passed as argv exact-match targets, but reject
/// anything tmux could parse as a window/pane suffix or that can't be a name.
fn valid_session_name(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c == ':' || c == '.' || c.is_control())
}

/// `send-keys -t <pane> -H <hex …>` lines carrying `bytes`, ≤ 256 input bytes
/// per line. Only hex tokens and the validated pane id appear in the output;
/// an invalid pane id yields no lines.
pub fn hex_keys(pane: &str, bytes: &[u8]) -> Vec<String> {
    if !valid_pane_id(pane) {
        return Vec::new();
    }
    bytes
        .chunks(KEYS_PER_LINE)
        .map(|chunk| {
            let mut line = format!("send-keys -t {pane} -H");
            for b in chunk {
                line.push_str(&format!(" {b:02x}"));
            }
            line
        })
        .collect()
}

/// Quick reply as two batches, mirroring the app's `tmux::send_keys`: the
/// text, then Enter (`0d`) on its own. One pty write of `text\r` can be read
/// as a paste by agent TUIs, so the message would not submit.
pub fn line_keys(pane: &str, text: &[u8]) -> (Vec<String>, Vec<String>) {
    (hex_keys(pane, text), hex_keys(pane, b"\r"))
}

fn resize_line(pane: &str, cols: u16, rows: u16) -> String {
    format!("resize-window -t {pane} -x {cols} -y {rows}")
}

fn unset_size_line(pane: &str) -> String {
    format!("set-option -wu -t {pane} window-size")
}

/// A repaint of the pane's current screen for a newly connected viewer.
#[derive(Debug, Clone)]
pub struct InitFrame {
    pub cols: u16,
    pub rows: u16,
    pub bytes: Vec<u8>,
}

/// What subscribers of a [`SessionHub`] receive.
#[derive(Debug, Clone)]
pub enum HubEvent {
    /// Raw pane output.
    Data(Bytes),
    /// The session (or its control client) ended: `"stopped"`.
    Closed(&'static str),
}

type Pending = Arc<StdMutex<VecDeque<oneshot::Sender<bool>>>>;

/// Pane id -> window size saved before the first fit, for every pane whose
/// fit is active. Lets quit restore sizes synchronously without the actors
/// (see [`Hubs::restore_fits_blocking`]).
type FitRegistry = Arc<StdMutex<HashMap<String, (u16, u16)>>>;

struct ScreenshotSend {
    file: PendingScreenshot,
    authorize: Box<dyn FnOnce() -> bool + Send>,
}

#[derive(Debug)]
pub(crate) enum SendLineError {
    /// Dequeued authorization check failed before any bytes were written.
    Unauthorized,
    /// Delivery failed before any bytes were written to tmux.
    NotWritten(io::Error),
    /// Delivery failed during or after writing to tmux.
    PartialOrUnknown(io::Error),
}

impl SendLineError {
    fn into_io(self) -> io::Error {
        match self {
            Self::Unauthorized => io::Error::new(io::ErrorKind::PermissionDenied, "unauthorized"),
            Self::NotWritten(e) | Self::PartialOrUnknown(e) => e,
        }
    }
}

enum Cmd {
    Send {
        lines: Vec<String>,
        done: oneshot::Sender<io::Result<()>>,
    },
    /// Optional attachment paste, then text and Enter in separate batches.
    /// One command keeps other input from landing in between.
    SendLine {
        batches: [Vec<String>; 3],
        screenshot: Option<ScreenshotSend>,
        done: oneshot::Sender<Result<(), SendLineError>>,
    },
    Fit {
        cols: u16,
        rows: u16,
        // The actor builds the guard: if the caller was cancelled, the guard
        // is dropped here (failed send) or inside the dropped receiver, and
        // either way queues a Release.
        done: oneshot::Sender<io::Result<FitGuard>>,
    },
    Release,
    Shutdown {
        done: Option<oneshot::Sender<()>>,
    },
}

/// One control-mode client for one session, shared by all viewers of it.
pub struct SessionHub {
    session: String,
    tmux: Tmux,
    pane: String,
    events: broadcast::Sender<HubEvent>,
    cmds: mpsc::UnboundedSender<Cmd>,
    closed: Arc<AtomicBool>,
}

impl SessionHub {
    /// Attach a control-mode client to exactly `session` and start streaming
    /// its active pane.
    pub async fn open(t: Tmux, session: String) -> io::Result<Arc<SessionHub>> {
        Self::open_in(t, session, FitRegistry::default()).await
    }

    async fn open_in(
        t: Tmux,
        session: String,
        registry: FitRegistry,
    ) -> io::Result<Arc<SessionHub>> {
        if !valid_session_name(&session) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid session name",
            ));
        }
        let target = format!("={session}:");
        let out = t
            .output(&[
                "display-message",
                "-p",
                "-t",
                &target,
                "#{pane_id} #{session_id}",
            ])
            .await?;
        let out = String::from_utf8_lossy(&out);
        let mut parts = out.split_whitespace();
        let pane = parts.next().unwrap_or("").to_string();
        let session_id = parts.next().unwrap_or("").to_string();
        // display-message exits 0 with empty output for an unknown target.
        if !valid_pane_id(&pane) || !session_id.starts_with('$') {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "tmux session not found",
            ));
        }

        let mut child = t
            .command()
            .args(["-C", "attach-session", "-t", &format!("={session}")])
            .args(["-f", "ignore-size"])
            // Inside tmux, $TMUX would trip the nesting check.
            .env_remove("TMUX")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;

        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        let closed = Arc::new(AtomicBool::new(false));
        let pending: Pending = Arc::default();

        tokio::spawn(read_loop(
            stdout,
            pane.clone(),
            session_id,
            events.clone(),
            pending.clone(),
            closed.clone(),
            cmds.downgrade(),
        ));
        tokio::spawn(
            Actor {
                tmux: t.clone(),
                pane: pane.clone(),
                child,
                stdin: Some(stdin),
                pending,
                closed: closed.clone(),
                cmds: cmds.downgrade(),
                fits: 0,
                saved: None,
                registry,
            }
            .run(cmd_rx),
        );

        Ok(Arc::new(SessionHub {
            session,
            tmux: t,
            pane,
            events,
            cmds,
            closed,
        }))
    }

    /// Subscribe to pane output. Subscribe *before* calling
    /// [`init_frame`](Self::init_frame) so nothing is missed (data arriving
    /// during the capture may repeat; that's harmless).
    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    /// The tmux session this hub is attached to.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// True once the control client has exited (`Closed` was broadcast).
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Current screen of the pane: clear, home, lines joined by CRLF, then
    /// the cursor position.
    pub async fn init_frame(&self) -> io::Result<InitFrame> {
        let capture = self
            .tmux
            .output(&["capture-pane", "-p", "-e", "-t", &self.pane])
            .await?;
        let info = self
            .tmux
            .output(&[
                "display-message",
                "-p",
                "-t",
                &self.pane,
                "#{pane_width} #{pane_height} #{cursor_x} #{cursor_y}",
            ])
            .await?;
        let nums: Vec<u16> = String::from_utf8_lossy(&info)
            .split_whitespace()
            .map(|n| n.parse::<u16>())
            .collect::<Result<_, _>>()
            .map_err(|_| io::Error::other("bad pane geometry"))?;
        let [cols, rows, x, y] = nums[..] else {
            return Err(io::Error::other("bad pane geometry"));
        };

        let mut bytes = b"\x1b[2J\x1b[H".to_vec();
        let body = capture.strip_suffix(b"\n").unwrap_or(&capture);
        for (i, line) in body.split(|&b| b == b'\n').enumerate() {
            if i > 0 {
                bytes.extend_from_slice(b"\r\n");
            }
            bytes.extend_from_slice(line);
        }
        bytes.extend_from_slice(format!("\x1b[{};{}H", y + 1, x + 1).as_bytes());
        Ok(InitFrame { cols, rows, bytes })
    }

    /// Type `bytes` into the pane (via `send-keys -H`).
    pub async fn send_input(&self, bytes: &[u8]) -> io::Result<()> {
        let lines = hex_keys(&self.pane, bytes);
        if lines.is_empty() {
            return Ok(());
        }
        let (done, rx) = oneshot::channel();
        self.request(Cmd::Send { lines, done })?;
        rx.await.map_err(|_| hub_gone())?
    }

    /// Type `text` and then press Enter as a separate write (quick reply).
    pub async fn send_line(&self, text: &[u8]) -> io::Result<()> {
        self.send_line_batches(text, &[], None)
            .await
            .map_err(SendLineError::into_io)
    }

    /// Deliver a screenshot and caption with a fresh authorization check in the actor.
    /// Codex recognizes the image when its path is pasted in a separate batch.
    pub(crate) async fn send_screenshot(
        &self,
        text: &str,
        file: PendingScreenshot,
        native_image: bool,
        authorize: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<(), SendLineError> {
        let path = file.path().to_string_lossy();
        let (text, paste) = if native_image {
            (format!(" {text}"), format!("\x1b[200~{path}\x1b[201~"))
        } else {
            (format!("{text} Screenshot file: {path:?}"), String::new())
        };
        let screenshot = ScreenshotSend { file, authorize: Box::new(authorize) };
        self.send_line_batches(text.as_bytes(), paste.as_bytes(), Some(screenshot)).await
    }

    async fn send_line_batches(
        &self,
        text: &[u8],
        paste: &[u8],
        screenshot: Option<ScreenshotSend>,
    ) -> Result<(), SendLineError> {
        let (text, enter) = line_keys(&self.pane, text);
        if enter.is_empty() {
            return Err(SendLineError::NotWritten(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bad pane",
            )));
        }
        let (done, rx) = oneshot::channel();
        let batches = [hex_keys(&self.pane, paste), text, enter];
        self.request(Cmd::SendLine {
            batches,
            screenshot,
            done,
        })
        .map_err(SendLineError::NotWritten)?;
        rx.await
            .map_err(|_| SendLineError::PartialOrUnknown(hub_gone()))?
    }

    /// Resize the window to `cols`x`rows` until the returned guard (and every
    /// other outstanding guard) is dropped; then the original size returns.
    pub async fn fit(&self, cols: u16, rows: u16) -> io::Result<FitGuard> {
        if !fit_size_ok(cols, rows) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad size"));
        }
        let (done, rx) = oneshot::channel();
        self.request(Cmd::Fit { cols, rows, done })?;
        rx.await.map_err(|_| hub_gone())?
    }

    /// Restore any fitted size and detach the control client.
    pub async fn shutdown(&self) {
        let (done, rx) = oneshot::channel();
        if self.cmds.send(Cmd::Shutdown { done: Some(done) }).is_ok() {
            let _ = rx.await;
        }
    }

    fn request(&self, cmd: Cmd) -> io::Result<()> {
        self.cmds.send(cmd).map_err(|_| hub_gone())
    }
}

impl Drop for SessionHub {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Shutdown { done: None });
    }
}

/// Largest window a phone may fit the pane to. A real phone or tablet is far
/// below this; anything bigger is a bad client and would make tmux allocate
/// a huge grid for every pane in the window.
pub const MAX_FIT_COLS: u16 = 500;
pub const MAX_FIT_ROWS: u16 = 300;

fn fit_size_ok(cols: u16, rows: u16) -> bool {
    (1..=MAX_FIT_COLS).contains(&cols) && (1..=MAX_FIT_ROWS).contains(&rows)
}

fn hub_gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "session hub closed")
}

/// Holds a window size set by [`SessionHub::fit`]. Dropping the last guard
/// restores the size recorded before the first fit.
pub struct FitGuard {
    cmds: mpsc::UnboundedSender<Cmd>,
}

impl Drop for FitGuard {
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Release);
    }
}

/// Reads the control client's stdout. Holds no reference to the hub.
///
/// Ends (and broadcasts `Closed`) on `%exit`, on EOF, and when the client is
/// moved to another session: with the user's `detach-on-destroy off`, killing
/// the attached session switches the client to some other (unmarked) session
/// instead of exiting it. The actor is then told to detach the client.
#[allow(clippy::too_many_arguments)]
async fn read_loop(
    stdout: ChildStdout,
    pane: String,
    session_id: String,
    events: broadcast::Sender<HubEvent>,
    pending: Pending,
    closed: Arc<AtomicBool>,
    cmds: mpsc::WeakUnboundedSender<Cmd>,
) {
    let mut reader = BufReader::new(stdout);
    let mut buf = Vec::new();
    // Inside %begin…%end the lines are command output, never notifications.
    let mut block: Option<bool> = None;
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = buf.strip_suffix(b"\n").unwrap_or(&buf);
        if let Some(ours) = block {
            let end = line.starts_with(b"%end ");
            if end || line.starts_with(b"%error ") {
                block = None;
                if ours {
                    let ack = pending.lock().unwrap().pop_front();
                    if let Some(ack) = ack {
                        let _ = ack.send(end);
                    }
                }
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix(b"%begin ") {
            // %begin <time> <number> <flags>; flags & 1 = sent by this client.
            let flags = rest
                .split(|&b| b == b' ')
                .nth(2)
                .and_then(|f| std::str::from_utf8(f).ok())
                .and_then(|f| f.parse::<u32>().ok())
                .unwrap_or(0);
            block = Some(flags & 1 == 1);
            continue;
        }
        match parse_line_bytes(line) {
            ControlEvent::Output { pane: p, data } if p == pane => {
                let _ = events.send(HubEvent::Data(Bytes::from(data)));
            }
            ControlEvent::Exit => break,
            ControlEvent::SessionChanged { id, .. } if id != session_id => break,
            _ => {}
        }
    }
    closed.store(true, Ordering::SeqCst);
    pending.lock().unwrap().clear();
    let _ = events.send(HubEvent::Closed(STOPPED));
    // Detach a client tmux moved elsewhere (no-op if it already exited).
    if let Some(cmds) = cmds.upgrade() {
        let _ = cmds.send(Cmd::Shutdown { done: None });
    }
}

/// Owns the control client's stdin and the fit refcount; serializes commands.
struct Actor {
    tmux: Tmux,
    pane: String,
    child: Child,
    stdin: Option<ChildStdin>,
    pending: Pending,
    /// Set by the reader when it stops: nothing will acknowledge commands.
    closed: Arc<AtomicBool>,
    /// Weak, so the actor doesn't keep its own channel open; used to build
    /// FitGuards.
    cmds: mpsc::WeakUnboundedSender<Cmd>,
    fits: usize,
    saved: Option<(u16, u16)>,
    registry: FitRegistry,
}

impl Actor {
    async fn send_line(
        &mut self,
        [paste, text, enter]: [Vec<String>; 3],
    ) -> Result<(), SendLineError> {
        let mut wrote_any = false;
        for batch in [paste, text].iter().filter(|batch| !batch.is_empty()) {
            match self.exec_tracking_write(batch).await {
                Ok(()) => wrote_any = true,
                Err((wrote, e)) => {
                    return Err(if wrote_any || wrote {
                        SendLineError::PartialOrUnknown(e)
                    } else {
                        SendLineError::NotWritten(e)
                    });
                }
            }
            tokio::time::sleep(ENTER_DELAY).await;
        }
        self.exec_tracking_write(&enter).await.map_err(|(wrote, e)| {
            if wrote_any || wrote {
                SendLineError::PartialOrUnknown(e)
            } else {
                SendLineError::NotWritten(e)
            }
        })
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Send { lines, done } => {
                    let _ = done.send(self.exec(&lines).await);
                }
                Cmd::SendLine {
                    batches,
                    screenshot,
                    done,
                } => {
                    let result = if let Some(ScreenshotSend { mut file, authorize }) = screenshot {
                        if done.is_closed() {
                            continue;
                        }
                        if !authorize() {
                            Err(SendLineError::Unauthorized)
                        } else {
                            // Ownership stays in the actor if the HTTP request is canceled.
                            // Keep the file from the moment a write might expose its path.
                            file.retain = true;
                            let result = self.send_line(batches).await;
                            file.retain = !matches!(result, Err(SendLineError::NotWritten(_)));
                            result
                        }
                    } else {
                        self.send_line(batches).await
                    };
                    let _ = done.send(result);
                }
                Cmd::Fit { cols, rows, done } => {
                    let res = match self.fit(cols, rows).await {
                        Ok(()) => match self.cmds.upgrade() {
                            Some(cmds) => Ok(FitGuard { cmds }),
                            None => {
                                self.release().await;
                                Err(hub_gone())
                            }
                        },
                        Err(e) => Err(e),
                    };
                    // On a cancelled caller the send fails and the returned
                    // guard drops here, queueing a Release.
                    let _ = done.send(res);
                }
                Cmd::Release => self.release().await,
                Cmd::Shutdown { done } => {
                    self.stop().await;
                    if let Some(done) = done {
                        let _ = done.send(());
                    }
                    break;
                }
            }
        }
        // Channel closed (hub and all guards gone) or shutdown: stop is
        // idempotent.
        self.stop().await;
    }

    async fn fit(&mut self, cols: u16, rows: u16) -> io::Result<()> {
        if self.fits == 0 {
            let out = self
                .tmux
                .output(&[
                    "display-message",
                    "-p",
                    "-t",
                    &self.pane,
                    "#{window_width} #{window_height}",
                ])
                .await?;
            let s = String::from_utf8_lossy(&out);
            let mut it = s.split_whitespace().map(|n| n.parse::<u16>());
            match (it.next(), it.next()) {
                (Some(Ok(w)), Some(Ok(h))) => self.saved = Some((w, h)),
                _ => return Err(io::Error::other("bad window size")),
            }
        }
        match self.exec(&[resize_line(&self.pane, cols, rows)]).await {
            Ok(()) => {
                self.fits += 1;
                if let Some(size) = self.saved {
                    let mut reg = self.registry.lock().unwrap_or_else(|e| e.into_inner());
                    reg.insert(self.pane.clone(), size);
                }
                Ok(())
            }
            Err(e) => {
                if self.fits == 0 {
                    self.saved = None;
                }
                Err(e)
            }
        }
    }

    async fn release(&mut self) {
        self.fits = self.fits.saturating_sub(1);
        if self.fits == 0 {
            self.restore().await;
        }
    }

    async fn restore(&mut self) {
        let Some((w, h)) = self.saved.take() else {
            return;
        };
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.pane);
        let lines = [resize_line(&self.pane, w, h), unset_size_line(&self.pane)];
        if self.exec(&lines).await.is_ok() {
            return;
        }
        // Control client gone but the session may live on: use argv.
        let (w, h) = (w.to_string(), h.to_string());
        let _ = self
            .tmux
            .output(&["resize-window", "-t", &self.pane, "-x", &w, "-y", &h])
            .await;
        let _ = self
            .tmux
            .output(&["set-option", "-wu", "-t", &self.pane, "window-size"])
            .await;
    }

    async fn stop(&mut self) {
        if self.stdin.is_none() {
            return;
        }
        if self.fits > 0 {
            self.fits = 0;
            self.restore().await;
        }
        // EOF on stdin makes the control client exit.
        self.stdin = None;
        if tokio::time::timeout(Duration::from_secs(1), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }

    /// Write control lines and wait for tmux to acknowledge each.
    async fn exec(&mut self, lines: &[String]) -> io::Result<()> {
        self.exec_tracking_write(lines).await.map_err(|(_, e)| e)
    }

    /// Write control lines, reporting whether any bytes were written on error.
    async fn exec_tracking_write(&mut self, lines: &[String]) -> Result<(), (bool, io::Error)> {
        let stdin = self.stdin.as_mut().ok_or_else(|| (false, hub_gone()))?;
        let mut acks = Vec::with_capacity(lines.len());
        let mut payload = Vec::new();
        for line in lines {
            debug_assert!(!line.contains('\n') && !line.is_empty());
            let (tx, rx) = oneshot::channel();
            self.pending.lock().unwrap().push_back(tx);
            acks.push(rx);
        }
        // Checked after queueing: the reader sets `closed` before it clears
        // `pending`, so either the ack sender is dropped or we see it here.
        if self.closed.load(Ordering::SeqCst) {
            self.pending.lock().unwrap().clear();
            return Err((false, hub_gone()));
        }
        for line in lines {
            payload.extend_from_slice(line.as_bytes());
            payload.push(b'\n');
        }
        stdin
            .write_all(&payload)
            .await
            .map_err(|e| (true, e))?;
        stdin.flush().await.map_err(|e| (true, e))?;
        let mut ok = true;
        for ack in acks {
            match tokio::time::timeout(ACK_TIMEOUT, ack).await {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => ok = false,
                Ok(Err(_)) => return Err((true, hub_gone())),
                Err(_) => return Err((true, io::Error::new(io::ErrorKind::TimedOut, "tmux ack"))),
            }
        }
        if ok {
            Ok(())
        } else {
            Err((true, io::Error::other("tmux rejected command")))
        }
    }
}

/// One hub per session, shared while anyone holds it.
pub struct Hubs {
    tmux: Tmux,
    map: tokio::sync::Mutex<HashMap<String, Weak<SessionHub>>>,
    fits: FitRegistry,
}

impl Hubs {
    pub fn new(t: Tmux) -> Hubs {
        Hubs {
            tmux: t,
            map: tokio::sync::Mutex::new(HashMap::new()),
            fits: FitRegistry::default(),
        }
    }

    /// Put every fitted window back to its saved size now, blocking, with
    /// argv `tmux resize-window` + `set-option -wu`, all within `budget`.
    /// For quit: tmux sessions outlive the app, so a size left behind by an
    /// actor that never got to run would stick. Spawns nothing when no fit
    /// is active. Must not be called from inside the tokio runtime.
    pub fn restore_fits_blocking(&self, budget: Duration) {
        let fits: Vec<(String, (u16, u16))> = self
            .fits
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .collect();
        if fits.is_empty() {
            return;
        }
        let deadline = Instant::now() + budget;
        for (pane, (w, h)) in fits {
            if Instant::now() >= deadline {
                break;
            }
            if !valid_pane_id(&pane) {
                continue;
            }
            let (w, h) = (w.to_string(), h.to_string());
            let mut resize = self.tmux.std_command();
            resize.args(["resize-window", "-t", &pane, "-x", &w, "-y", &h]);
            run_until(resize, deadline);
            let mut unset = self.tmux.std_command();
            unset.args(["set-option", "-wu", "-t", &pane, "window-size"]);
            run_until(unset, deadline);
        }
    }

    /// The live hub for `session`, opening one if none is held or the
    /// previous one has closed.
    pub async fn get(&self, session: &str) -> io::Result<Arc<SessionHub>> {
        let mut map = self.map.lock().await;
        if let Some(hub) = map.get(session).and_then(Weak::upgrade) {
            if !hub.is_closed() {
                return Ok(hub);
            }
        }
        map.retain(|_, w| w.strong_count() > 0);
        let hub =
            SessionHub::open_in(self.tmux.clone(), session.to_string(), self.fits.clone()).await?;
        map.insert(session.to_string(), Arc::downgrade(&hub));
        Ok(hub)
    }

    /// Forget and shut down the hub for `session` (e.g. when unmarked).
    pub async fn close(&self, session: &str) {
        let hub = self
            .map
            .lock()
            .await
            .remove(session)
            .and_then(|w| w.upgrade());
        if let Some(hub) = hub {
            hub.shutdown().await;
        }
    }
}

/// Run `cmd` to completion or until `deadline` (then kill it); true on
/// success. Blocking.
fn run_until(mut cmd: std::process::Command, deadline: Instant) -> bool {
    let child = cmd
        .env_remove("TMUX")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        return false;
    };
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_size_bounds() {
        assert!(fit_size_ok(1, 1));
        assert!(fit_size_ok(MAX_FIT_COLS, MAX_FIT_ROWS));
        assert!(!fit_size_ok(0, 40));
        assert!(!fit_size_ok(80, 0));
        assert!(!fit_size_ok(MAX_FIT_COLS + 1, 40));
        assert!(!fit_size_ok(80, MAX_FIT_ROWS + 1));
        assert!(!fit_size_ok(10_000, 10_000));
    }

    #[test]
    fn restore_fits_blocking_without_fits_is_instant() {
        let hubs = Hubs::new(Tmux {
            socket: Some(format!("oo-remote-unused-{}", std::process::id())),
        });
        let t = Instant::now();
        hubs.restore_fits_blocking(Duration::from_secs(1));
        assert!(t.elapsed() < Duration::from_millis(20));
    }

    #[test]
    fn decode_octal_escapes() {
        assert_eq!(decode_output("a\\033[1mb\\134c"), b"a\x1b[1mb\\c".to_vec());
    }

    #[test]
    fn parse_output_line() {
        match parse_line("%output %12 hi\\015\\012") {
            ControlEvent::Output { pane, data } => {
                assert_eq!(pane, "%12");
                assert_eq!(data, b"hi\r\n".to_vec());
            }
            _ => panic!("expected Output"),
        }
        assert!(matches!(parse_line("%exit"), ControlEvent::Exit));
        assert!(matches!(parse_line("%begin 1 2 0"), ControlEvent::Other));
    }

    #[test]
    fn hex_keys_chunks_utf8() {
        assert_eq!(
            hex_keys("%3", "é🙂".as_bytes()),
            vec!["send-keys -t %3 -H c3 a9 f0 9f 99 82".to_string()]
        );
        let input: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        let lines = hex_keys("%3", &input);
        assert_eq!(lines.len(), 3);
        let mut back = Vec::new();
        for l in &lines {
            let hex = l.strip_prefix("send-keys -t %3 -H ").unwrap();
            for tok in hex.split(' ') {
                back.push(u8::from_str_radix(tok, 16).unwrap());
            }
        }
        assert_eq!(back, input);
    }

    #[test]
    fn hex_keys_never_contains_raw_input() {
        let lines = hex_keys("%3", b"; kill-server");
        assert!(!lines.is_empty());
        for l in &lines {
            let hex = l.strip_prefix("send-keys -t %3 -H ").unwrap();
            for tok in hex.split(' ') {
                assert_eq!(tok.len(), 2);
                assert!(tok.bytes().all(|b| b.is_ascii_hexdigit()));
            }
            assert!(!l.contains("kill"));
        }
    }

    #[test]
    fn decode_edge_cases() {
        assert_eq!(decode_output("a\\"), b"a\\".to_vec());
        assert_eq!(decode_output("\\9x"), b"\\9x".to_vec());
        assert_eq!(decode_output("\\400"), b"\\400".to_vec());
        assert_eq!(decode_output("\\377\\\\"), vec![0xff, b'\\']);
        assert_eq!(decode_output("\\01"), b"\\01".to_vec());
    }

    #[test]
    fn pane_ids_are_validated() {
        assert!(hex_keys("%3; kill-server", b"a").is_empty());
        assert!(hex_keys("3", b"a").is_empty());
        assert!(hex_keys("%", b"a").is_empty());
        assert!(hex_keys("%3", b"").is_empty());
        assert_eq!(parse_line("%output %x hi"), ControlEvent::Other);
        assert_eq!(parse_line("%outputx %1 hi"), ControlEvent::Other);
        assert_eq!(
            parse_line_bytes(b"%output %1 \xff\\015"),
            ControlEvent::Output {
                pane: "%1".into(),
                data: vec![0xff, b'\r']
            }
        );
    }

    #[test]
    fn quick_reply_enter_is_its_own_batch() {
        let (text, enter) = line_keys("%3", b"hi");
        assert_eq!(text, vec!["send-keys -t %3 -H 68 69".to_string()]);
        assert_eq!(enter, vec!["send-keys -t %3 -H 0d".to_string()]);
        let (text, enter) = line_keys("%3", b"");
        assert!(text.is_empty());
        assert_eq!(enter.len(), 1);
        assert_eq!(line_keys("bad", b"x"), (vec![], vec![]));
    }

    #[test]
    fn parse_session_changed() {
        assert_eq!(
            parse_line("%session-changed $1 b"),
            ControlEvent::SessionChanged {
                id: "$1".into(),
                name: "b".into()
            }
        );
        assert_eq!(
            parse_line("%session-changed $0 a b"),
            ControlEvent::SessionChanged {
                id: "$0".into(),
                name: "a b".into()
            }
        );
        assert_eq!(parse_line("%session-changed x"), ControlEvent::Other);
        assert_eq!(parse_line("%sessions-changed"), ControlEvent::Other);
    }

    #[test]
    fn hex_keys_line_shape() {
        let lines = hex_keys("%3", &[0u8; 256]);
        assert_eq!(lines.len(), 1);
        assert_eq!(hex_keys("%3", &[0u8; 257]).len(), 2);
    }

    #[tokio::test]
    async fn screenshot_cleanup_tracks_whether_the_path_may_have_been_written() {
        use std::future::Future;
        use std::task::{Context, Waker};
        use tokio::io::AsyncReadExt;

        let dir = tempfile::tempdir().unwrap();
        let save = || {
            let file = crate::store::save_screenshot(dir.path(), "png", b"test image").unwrap();
            let path = file.path().to_path_buf();
            (file, path)
        };
        let mut child = tokio::process::Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let closed = Arc::new(AtomicBool::new(false));
        let pending = Pending::default();
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(8);
        let hub = SessionHub {
            session: "s_test".into(),
            tmux: Tmux::default(),
            pane: "%1".into(),
            events,
            cmds: cmds.clone(),
            closed: closed.clone(),
        };
        let (canceled, canceled_path) = save();
        let mut send = Box::pin(hub.send_screenshot("hi", canceled, true, || true));
        assert!(send.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        assert!(canceled_path.is_file());
        drop(send);

        tokio::spawn(
            Actor {
                tmux: Tmux::default(),
                pane: "%1".into(),
                child,
                stdin: Some(stdin),
                pending: pending.clone(),
                closed: closed.clone(),
                cmds: cmds.downgrade(),
                fits: 0,
                saved: None,
                registry: FitRegistry::default(),
            }
            .run(cmd_rx),
        );

        let (unauthorized, unauthorized_path) = save();
        let res = hub.send_screenshot("hi", unauthorized, true, || false).await;
        assert!(matches!(res, Err(SendLineError::Unauthorized)));
        assert!(!unauthorized_path.exists());
        assert!(!canceled_path.exists());

        // The hub closes after queuing but before the first write.
        let (closed_hub, closed_path) = save();
        let actor_closed = closed.clone();
        let res = hub
            .send_screenshot("hi", closed_hub, false, move || {
                actor_closed.store(true, Ordering::SeqCst);
                true
            })
            .await;
        assert!(matches!(res, Err(SendLineError::NotWritten(_))));
        assert!(!closed_path.exists());

        // Once the path was written, canceling the request must preserve the file.
        closed.store(false, Ordering::SeqCst);
        let (written, written_path) = save();
        let mut send = Box::pin(hub.send_screenshot("hi", written, true, || true));
        assert!(send.as_mut().poll(&mut Context::from_waker(Waker::noop())).is_pending());
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(2), stdout.read_line(&mut line))
            .await
            .unwrap()
            .unwrap();
        let paste = format!("\x1b[200~{}\x1b[201~", written_path.display());
        assert_eq!(line.trim_end(), hex_keys("%1", paste.as_bytes())[0]);
        drop(send);
        // Stop the send before other batches by simulating a failed tmux acknowledgment.
        pending.lock().unwrap().pop_front().unwrap().send(false).unwrap();
        hub.shutdown().await;
        assert!(written_path.is_file());
        let mut received = Vec::new();
        stdout.read_to_end(&mut received).await.unwrap();
        assert!(received.is_empty(), "expected zero bytes written: {received:?}");
    }
}
