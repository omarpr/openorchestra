//! `GET /api/term/{id}`: the live terminal WebSocket.
//!
//! Protocol (all JSON frames are text):
//! - Upgrade requires `Origin == https://<cert SAN>:<port>`, else 403.
//! - Client's first frame within 5s: `{"type":"auth","token":"…"}`, else
//!   close 4401.
//! - Server: `{"type":"init","cols","rows","read_only"}`, then one binary
//!   frame with the screen repaint (starts with `ESC[0m`), then binary
//!   frames of live output. `init` + repaint repeat if the client lagged.
//! - Client: `{"type":"input","data":"…"}` and
//!   `{"type":"fit","on":bool,"cols","rows"}` (ignored for read-only).
//! - Server pings every 20s; a socket with no frame from the client
//!   (pongs count) for 45s is dropped, releasing its fit. Sends that can't
//!   complete within that limit drop the socket too.
//! - Server ends with `{"type":"closed","reason":"stopped"|"unmarked"|
//!   "revoked"|"disabled"}` then a close frame (1000).

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::api::{error, AppState, HUB_OPEN_TIMEOUT};
use crate::server::Shared;
use crate::tmux_bridge::{FitGuard, HubEvent, SessionHub};

const AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// Max decoded `input` data per frame (bytes).
pub(crate) const MAX_INPUT: usize = 64 * 1024;
/// Transport cap (JSON escaping can inflate `data` several times).
const MAX_MESSAGE: usize = 512 * 1024;

pub(crate) const CLOSE_NORMAL: u16 = 1000;
pub(crate) const CLOSE_TOO_BIG: u16 = 1009;
pub(crate) const CLOSE_INTERNAL: u16 = 1011;
pub(crate) const CLOSE_UNAUTHORIZED: u16 = 4401;

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ClientMsg {
    Auth {
        token: String,
    },
    Input {
        data: String,
    },
    Fit {
        on: bool,
        #[serde(default)]
        cols: u16,
        #[serde(default)]
        rows: u16,
    },
}

pub(crate) async fn term(
    State(s): AppState,
    Path(id): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .unwrap_or("");
    if !s.origin_allowed(origin) {
        return error(StatusCode::FORBIDDEN, "bad_origin");
    }
    upgrade
        .max_message_size(MAX_MESSAGE)
        .max_frame_size(MAX_MESSAGE)
        .on_upgrade(move |socket| async move {
            let _open = s.open_streams.clone();
            Term { s, id, socket }.run().await;
        })
        .into_response()
}

struct Term {
    s: Arc<Shared>,
    id: String,
    socket: WebSocket,
}

/// Bound on the goodbye frames to a client that may be gone.
const FAREWELL: Duration = Duration::from_secs(2);

enum End {
    /// Send `{"type":"closed","reason"}` then close 1000.
    Reason(&'static str),
    Code(u16, &'static str),
    /// The client went away.
    Gone,
}

impl Term {
    async fn run(mut self) {
        let end = self.serve().await;
        match end {
            End::Reason(reason) => {
                let msg = json!({ "type": "closed", "reason": reason }).to_string();
                let _ = tokio::time::timeout(FAREWELL, self.socket.send(Message::text(msg))).await;
                self.close(CLOSE_NORMAL, reason).await;
            }
            End::Code(code, reason) => self.close(code, reason).await,
            End::Gone => {}
        }
    }

    async fn close(&mut self, code: u16, reason: &'static str) {
        let frame = Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        }));
        let _ = tokio::time::timeout(FAREWELL, self.socket.send(frame)).await;
    }

    /// Send, giving up (client gone) if it can't complete within the idle
    /// limit: with a vanished peer and steady output the kernel buffer fills
    /// and a plain send would block the loop, and the idle check, forever.
    async fn send(&mut self, msg: Message) -> Result<(), End> {
        let (_, idle) = self.s.ws_keepalive();
        match tokio::time::timeout(idle, self.socket.send(msg)).await {
            Ok(Ok(())) => Ok(()),
            _ => Err(End::Gone),
        }
    }

    async fn serve(&mut self) -> End {
        // Subscribe before authenticating: a revoke/unmark published while
        // the hub opens must still be seen as a change.
        let mut stop = self.s.shutdown.subscribe();
        let mut file_rx = self.s.file.subscribe();
        // 1. Auth frame.
        let token = match tokio::time::timeout(AUTH_TIMEOUT, self.socket.recv()).await {
            Ok(Some(Ok(Message::Text(t)))) => match serde_json::from_str(t.as_str()) {
                Ok(ClientMsg::Auth { token }) => token,
                _ => return End::Code(CLOSE_UNAUTHORIZED, "auth required"),
            },
            Ok(None | Some(Err(_))) => return End::Gone,
            _ => return End::Code(CLOSE_UNAUTHORIZED, "auth required"),
        };
        let Some((device, file)) = crate::api::authenticate(&self.s, &token) else {
            return End::Code(CLOSE_UNAUTHORIZED, "unauthorized");
        };
        let mut read_only = device.read_only;

        // 2. id -> session, only through the current marks.
        let Some(session) = self.s.session_for(&file, &self.id) else {
            return End::Reason("unmarked");
        };
        let hub = match tokio::time::timeout(HUB_OPEN_TIMEOUT, self.s.hubs.get(&session)).await {
            Ok(Ok(hub)) => hub,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return End::Reason("stopped")
            }
            Ok(Err(e)) => {
                eprintln!("oo-remote: open session failed: {e}");
                return End::Code(CLOSE_INTERNAL, "unavailable");
            }
            Err(_) => return End::Code(CLOSE_INTERNAL, "unavailable"),
        };

        // Opening can take seconds: re-check the mapping before streaming.
        if let Err(end) = self.recheck(&device.id, &session) {
            return end;
        }

        // 3. Subscribe before capturing so nothing is missed.
        let mut events = hub.subscribe();
        if let Err(end) = self.send_init(&hub, read_only).await {
            return end;
        }

        let mut fit: Option<FitGuard> = None;
        let (ping_every, idle) = self.s.ws_keepalive();
        let mut ping = tokio::time::interval_at(Instant::now() + ping_every, ping_every);
        let mut last_rx = Instant::now();
        loop {
            tokio::select! {
                // Order: stop, liveness, access changes, then client frames
                // before hub output so a flood of output never starves input
                // (e.g. Ctrl-C to a runaway loop).
                biased;
                _ = crate::server::stopped(&mut stop) => {
                    drop(fit.take());
                    // Restore any fitted size and detach before the runtime
                    // goes away.
                    // Every viewer awaits it: a second call returns at once.
                    hub.shutdown().await;
                    return End::Reason("disabled");
                }
                // Nothing from the phone (not even a pong) for `idle`: it is
                // gone. Returning drops `fit`, which restores the window.
                _ = tokio::time::sleep_until(last_rx + idle) => return End::Gone,
                _ = ping.tick() => {
                    if let Err(end) = self.send(Message::Ping(Default::default())).await {
                        return end;
                    }
                }
                r = file_rx.changed() => {
                    if r.is_err() {
                        continue;
                    }
                    let f = file_rx.borrow_and_update().clone();
                    let Some(d) = f.devices.iter().find(|d| d.id == device.id) else {
                        return End::Reason("revoked");
                    };
                    if f.mark_by_id(&self.id).is_none() {
                        return End::Reason("unmarked");
                    }
                    if d.read_only != read_only {
                        read_only = d.read_only;
                        if read_only {
                            fit = None;
                        }
                    }
                }
                msg = self.socket.recv() => {
                    if matches!(msg, Some(Ok(_))) {
                        last_rx = Instant::now();
                    }
                    let text = match msg {
                        Some(Ok(Message::Text(t))) => t,
                        Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return End::Gone,
                        Some(Ok(_)) => continue,
                    };
                    match serde_json::from_str::<ClientMsg>(text.as_str()) {
                        Ok(ClientMsg::Input { data }) => {
                            if data.len() > MAX_INPUT {
                                return End::Code(CLOSE_TOO_BIG, "input too large");
                            }
                            if !read_only {
                                match self.recheck(&device.id, &session) {
                                    Err(end) => return end,
                                    Ok(false) => {
                                        read_only = true;
                                        fit = None;
                                        continue;
                                    }
                                    Ok(true) => {}
                                }
                                if let Err(e) = hub.send_input(data.as_bytes()).await {
                                    eprintln!("oo-remote: input failed: {e}");
                                }
                            }
                        }
                        Ok(ClientMsg::Fit { on, cols, rows }) if !read_only => {
                            if on {
                                match self.recheck(&device.id, &session) {
                                    Err(end) => return end,
                                    Ok(false) => {
                                        read_only = true;
                                        fit = None;
                                        continue;
                                    }
                                    Ok(true) => {}
                                }
                                // Take the new size before releasing the old.
                                match hub.fit(cols, rows).await {
                                    Ok(g) => fit = Some(g),
                                    Err(e) => eprintln!("oo-remote: fit failed: {e}"),
                                }
                            } else {
                                fit = None;
                            }
                        }
                        _ => {}
                    }
                }
                ev = events.recv() => match ev {
                    Ok(HubEvent::Data(bytes)) => {
                        if let Err(end) = self.send(Message::Binary(bytes)).await {
                            return end;
                        }
                    }
                    Ok(HubEvent::Closed(_)) | Err(RecvError::Closed) => {
                        return End::Reason("stopped");
                    }
                    Err(RecvError::Lagged(_)) => {
                        events = events.resubscribe();
                        if let Err(end) = self.send_init(&hub, read_only).await {
                            return end;
                        }
                    }
                },
            }
        }
    }

    /// Fresh read of remote.json before input/fit takes effect, so the
    /// watcher's latency never lets keystrokes through after an unmark or
    /// revoke. `Ok(false)`: the device became read-only (ignore the frame).
    fn recheck(&self, device_id: &str, session: &str) -> Result<bool, End> {
        let file = self.s.current_file();
        let Some(d) = file.devices.iter().find(|d| d.id == device_id) else {
            return Err(End::Reason("revoked"));
        };
        if self.s.session_for(&file, &self.id).as_deref() != Some(session) {
            return Err(End::Reason("unmarked"));
        }
        Ok(!d.read_only)
    }

    /// `init` JSON then the repaint (prefixed with an SGR reset).
    async fn send_init(&mut self, hub: &SessionHub, read_only: bool) -> Result<(), End> {
        let init = match hub.init_frame().await {
            Ok(i) => i,
            Err(_) => return Err(End::Reason("stopped")),
        };
        let head = json!({
            "type": "init",
            "cols": init.cols,
            "rows": init.rows,
            "read_only": read_only,
        });
        let mut bytes = b"\x1b[0m".to_vec();
        bytes.extend_from_slice(&init.bytes);
        self.send(Message::text(head.to_string())).await?;
        self.send(Message::Binary(bytes.into())).await
    }
}
