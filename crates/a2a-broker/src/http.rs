//! Blocking localhost HTTP server for Agent Cards + JSON-RPC (+ SSE subscribe).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::audit::AuditEvent;
use crate::bridge::{BridgeEvent, InboxDelivery, InboxMessage};
use crate::card::{agent_card_for_member, orchestrator_card};
use crate::protocol::{
    Artifact, JsonRpcRequest, JsonRpcResponse, Message, MessagePart, Role, SendMessageParams,
    Task, TaskState, TaskStatus,
};
use crate::BrokerState;

/// Running broker: bind address + shutdown handle.
pub struct BrokerHandle {
    pub addr: SocketAddr,
    pub state: Arc<BrokerState>,
    shutdown: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    drain: Option<JoinHandle<()>>,
}

impl BrokerHandle {
    /// Stop the accept/drain threads and wait for them to exit.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        // Idempotent: Drop and `shutdown()` both call this; Options make the
        // second pass a no-op after joins are taken.
        if self.join.is_none() && self.drain.is_none() {
            return;
        }
        self.shutdown.store(true, Ordering::SeqCst);
        // Wake the accept loop.
        let _ = TcpStream::connect(self.addr);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
        if let Some(j) = self.drain.take() {
            let _ = j.join();
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn api_key(&self) -> &str {
        &self.state.config.api_key
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

pub struct BrokerServer;

impl BrokerServer {
    /// Bind and serve on a background thread. Use port `0` in `config.bind`
    /// to let the OS assign an ephemeral port.
    pub fn start(mut state: BrokerState) -> std::io::Result<BrokerHandle> {
        let listener = TcpListener::bind(state.config.bind)?;
        let addr = listener.local_addr()?;
        state.config.bind = addr;
        let state = Arc::new(state);

        let shutdown = Arc::new(AtomicBool::new(false));
        let state_accept = Arc::clone(&state);
        let shutdown_accept = Arc::clone(&shutdown);

        let join = thread::Builder::new()
            .name("a2a-broker".into())
            .spawn(move || {
                // One thread per connection, so the number of them is capped:
                // otherwise any local process could open connections until the
                // app runs out of threads.
                let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                for stream in listener.incoming() {
                    if shutdown_accept.load(Ordering::SeqCst) {
                        break;
                    }
                    match stream {
                        Ok(stream) => {
                            if live.load(Ordering::SeqCst) >= MAX_CONNECTIONS {
                                drop(stream);
                                continue;
                            }
                            live.fetch_add(1, Ordering::SeqCst);
                            let st = Arc::clone(&state_accept);
                            let live_conn = Arc::clone(&live);
                            let spawned = thread::Builder::new()
                                .name("a2a-conn".into())
                                .spawn(move || {
                                    if let Err(e) = handle_connection(stream, st) {
                                        log_warn(&format!("a2a conn: {e}"));
                                    }
                                    live_conn.fetch_sub(1, Ordering::SeqCst);
                                });
                            if spawned.is_err() {
                                live.fetch_sub(1, Ordering::SeqCst);
                            }
                        }
                        Err(_) => break,
                    }
                }
            })?;

        let state_drain = Arc::clone(&state);
        let shutdown_drain = Arc::clone(&shutdown);
        let drain = thread::Builder::new()
            .name("a2a-drain".into())
            .spawn(move || {
                while !shutdown_drain.load(Ordering::SeqCst) {
                    drain_bridge(&state_drain);
                    thread::sleep(Duration::from_millis(200));
                }
            })?;

        Ok(BrokerHandle {
            addr,
            state,
            shutdown,
            join: Some(join),
            drain: Some(drain),
        })
    }
}

fn log_warn(msg: &str) {
    eprintln!("[a2a-broker] {msg}");
}

fn now_rfc3339() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_utc(secs)
}

/// `secs` since the epoch as `YYYY-MM-DDTHH:MM:SSZ`. The name always promised
/// RFC 3339, but it used to return bare epoch seconds.
fn rfc3339_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Howard Hinnant's days-from-civil, inverted.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem / 60) % 60,
        rem % 60
    )
}

fn drain_bridge(state: &BrokerState) {
    while let Some(ev) = state.bridge.try_recv_completion() {
        let ts = now_rfc3339();
        let mut tasks = match state.tasks.lock() {
            Ok(t) => t,
            Err(_) => return,
        };
        let (a2a_task_id, new_state, text, label, summary) = match ev {
            BridgeEvent::Completed {
                a2a_task_id,
                summary,
            } => (
                a2a_task_id,
                TaskState::Completed,
                summary.clone(),
                "completed",
                Some(summary),
            ),
            BridgeEvent::Failed { a2a_task_id, error } => {
                (a2a_task_id, TaskState::Failed, error, "failed", None)
            }
            BridgeEvent::Rejected { a2a_task_id, reason } => {
                (a2a_task_id, TaskState::Rejected, reason, "rejected", None)
            }
        };
        // A terminal state is final: a repeated or late complete/reject must
        // not flip it, duplicate the summary artifact, or re-notify. Read
        // before anything below touches the task.
        let (prev_terminal, tenant, context_id) = match tasks.get(&a2a_task_id) {
            Some(t) => (
                matches!(
                    t.status.state,
                    TaskState::Completed
                        | TaskState::Failed
                        | TaskState::Canceled
                        | TaskState::Rejected
                ),
                t.tenant.clone(),
                t.context_id.clone(),
            ),
            None => continue,
        };
        if prev_terminal {
            drop(tasks);
            if let Ok(mut audit) = state.audit.lock() {
                let _ = audit.append(&AuditEvent {
                    ts: &ts,
                    kind: "task.result_ignored",
                    oo_task_id: &state.config.oo_task_id,
                    tenant: Some(&tenant),
                    a2a_task_id: Some(&a2a_task_id),
                    detail: Some(serde_json::json!({ "state": label })),
                });
            }
            continue;
        }
        if let Some(summary) = summary {
            if let Some(task) = tasks.get_mut(&a2a_task_id) {
                task.artifacts.push(Artifact {
                    artifact_id: uuid::Uuid::new_v4().to_string(),
                    name: Some("summary".into()),
                    parts: vec![MessagePart::text(summary)],
                });
            }
        }
        let _ = tasks.set_state(
            &a2a_task_id,
            new_state,
            Some(Message {
                message_id: uuid::Uuid::new_v4().to_string(),
                context_id: Some(state.config.oo_task_id.clone()),
                task_id: Some(a2a_task_id.clone()),
                role: Role::Agent,
                parts: vec![MessagePart::text(text.clone())],
            }),
            ts.clone(),
        );
        // Never hold the task lock across bridge IO or the inbox hook.
        drop(tasks);
        if new_state == TaskState::Completed {
            if let Ok(mut audit) = state.audit.lock() {
                let _ = audit.append(&AuditEvent {
                    ts: &ts,
                    kind: "task.completed",
                    oo_task_id: &state.config.oo_task_id,
                    tenant: None,
                    a2a_task_id: Some(&a2a_task_id),
                    detail: None,
                });
            }
        }
        state.notify_change();

        let coordinator = state.config.coordinator_tenant.clone();
        // The coordinator completing a message sent to it: pushing that back
        // would echo its own answer into its inbox.
        if tenant == coordinator {
            continue;
        }
        let repo = state
            .member(&tenant)
            .map(|m| m.repo.clone())
            .unwrap_or_else(|| tenant.clone());
        let msg = InboxMessage {
            a2a_task_id: a2a_task_id.clone(),
            context_id,
            from_tenant: Some(tenant),
            text: format!("{label} by {repo}: {text}"),
            created_at: ts,
            reply: true,
        };
        match state.bridge.enqueue(&coordinator, &msg) {
            Ok(()) => state.notify_inbox(InboxDelivery {
                oo_task_id: state.config.oo_task_id.clone(),
                tenant: coordinator,
                message: msg,
            }),
            Err(e) => log_warn(&format!(
                "push {label} reply for {a2a_task_id} to {coordinator}: {e}"
            )),
        }
    }
}

/// Longest request or header line accepted, in bytes.
const MAX_LINE: u64 = 8 * 1024;
/// Most header lines accepted on one request.
const MAX_HEADERS: usize = 64;
/// Largest request body accepted. JSON-RPC messages between agents are text;
/// a megabyte is generous.
const MAX_BODY: usize = 1024 * 1024;
/// How long a client may take to send its request before it is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Most connections served at once (SSE subscribers hold theirs open).
const MAX_CONNECTIONS: usize = 64;

/// One line, at most [`MAX_LINE`] bytes. `None` when the line is too long:
/// an unbounded `read_line` grows its buffer for as long as a client keeps
/// sending bytes without a newline.
fn read_line_capped<R: BufRead>(reader: &mut R) -> std::io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader.by_ref().take(MAX_LINE + 1).read_until(b'\n', &mut buf)?;
    if n as u64 > MAX_LINE {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn handle_connection(mut stream: TcpStream, state: Arc<BrokerState>) -> std::io::Result<()> {
    // Everything up to the key check is reachable by any local process, so
    // every read before it is bounded: line length, header count, body size,
    // and time. The body is read only after the key has been checked — the
    // declared Content-Length used to be allocated up front, so one request
    // claiming a huge body aborted the whole app.
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(request_line) = read_line_capped(&mut reader)? else {
        write_response(&mut stream, 414, "text/plain", b"request line too long")?;
        return Ok(());
    };
    let mut content_length = 0usize;
    let mut auth = String::new();
    let mut content_type = String::new();
    let mut header_count = 0usize;
    loop {
        let Some(line) = read_line_capped(&mut reader)? else {
            write_response(&mut stream, 431, "text/plain", b"header too long")?;
            return Ok(());
        };
        if line == "\r\n" || line == "\n" || line.is_empty() {
            break;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            write_response(&mut stream, 431, "text/plain", b"too many headers")?;
            return Ok(());
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("authorization") {
                auth = value.trim().to_string();
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = value.trim().to_ascii_lowercase();
            }
        }
    }

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        write_response(&mut stream, 400, "text/plain", b"bad request")?;
        return Ok(());
    }
    let method = parts[0];
    let path = parts[1];

    // Only the bare health check is open. This used to match any path
    // *ending* in "/health", which let an unauthenticated request reach
    // routing for paths it had no business reaching.
    if !authorized(&auth, &state.config.api_key) && path != "/health" {
        // Agent card discovery still requires auth in v1 (localhost key).
        write_response(&mut stream, 401, "text/plain", b"unauthorized")?;
        return Ok(());
    }

    if content_length > MAX_BODY {
        write_response(&mut stream, 413, "text/plain", b"body too large")?;
        return Ok(());
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    // A subscription then streams for as long as the client listens; only the
    // request itself was on the clock.
    stream.set_read_timeout(None)?;

    if method == "GET" && path == "/health" {
        write_response(&mut stream, 200, "text/plain", b"ok")?;
        return Ok(());
    }

    // Root card = coordinator
    if method == "GET" && (path == "/.well-known/agent-card.json" || path == "/.well-known/agent.json")
    {
        let home = state
            .config
            .members
            .iter()
            .find(|m| m.is_coordinator)
            .or_else(|| state.config.members.first());
        let Some(home) = home else {
            write_response(&mut stream, 404, "text/plain", b"no coordinator")?;
            return Ok(());
        };
        let siblings: Vec<_> = state
            .config
            .members
            .iter()
            .filter(|m| !m.is_coordinator)
            .cloned()
            .collect();
        let card = orchestrator_card(
            &state.base_url(),
            &state.config.task_name,
            &state.config.task_slug,
            home,
            &siblings,
        );
        let body = serde_json::to_vec_pretty(&card).unwrap_or_default();
        write_response(&mut stream, 200, "application/json", &body)?;
        return Ok(());
    }

    // Tenant card: /t/<worktree_id>/.well-known/agent-card.json
    // Tenant may contain `/` (encoded as %2F) or literal `/` from naive clients.
    if method == "GET" {
        if let Some(tenant) = tenant_from_card_path(path) {
            if let Some(member) = state.member(&tenant) {
                let card = agent_card_for_member(
                    &state.base_url(),
                    &state.config.task_name,
                    &state.config.task_slug,
                    member,
                );
                let body = serde_json::to_vec_pretty(&card).unwrap_or_default();
                write_response(&mut stream, 200, "application/json", &body)?;
                return Ok(());
            }
        }
    }

    // JSON-RPC: /jsonrpc or /t/<tenant>/jsonrpc
    if method == "POST" && (path == "/jsonrpc" || path.ends_with("/jsonrpc")) {
        let tenant = tenant_from_jsonrpc_path(path);
        let req: JsonRpcRequest = match serde_json::from_slice(&body) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::err(None, -32700, format!("parse error: {e}"));
                let b = serde_json::to_vec(&resp).unwrap_or_default();
                write_response(&mut stream, 200, "application/json", &b)?;
                return Ok(());
            }
        };

        // Streaming subscribe uses SSE response.
        if req.method == "tasks/subscribe"
            || req.method == "SubscribeToTask"
            || req.method == "message/stream"
            || req.method == "SendStreamingMessage"
        {
            return handle_stream(&mut stream, &state, &req, tenant.as_deref());
        }

        let resp = dispatch_rpc(&state, &req, tenant.as_deref());
        let b = serde_json::to_vec(&resp).unwrap_or_default();
        write_response(&mut stream, 200, "application/json", &b)?;
        return Ok(());
    }

    let is_form = content_type.starts_with("application/x-www-form-urlencoded");

    // Plain-form send, for the shell helpers: `curl --data-urlencode text=…`
    // does the encoding, so they need neither python nor jq to build JSON.
    if method == "POST" && is_form {
        if let Some(tenant) = path.strip_prefix("/t/").and_then(|r| r.strip_suffix("/send")) {
            let tenant = crate::decode_tenant(tenant);
            let text = form_value(&body, "text").unwrap_or_default();
            let params = serde_json::json!({
                "message": {
                    "message_id": "",
                    "role": "user",
                    "parts": [{ "text": text }]
                }
            });
            let resp = match handle_send_message(&state, &params, Some(&tenant)) {
                Ok(v) => JsonRpcResponse::ok(None, v),
                Err(e) => JsonRpcResponse::err(None, -32000, e),
            };
            let b = serde_json::to_vec(&resp).unwrap_or_default();
            write_response(&mut stream, 200, "application/json", &b)?;
            return Ok(());
        }
        if path == "/v1/status" {
            let id = form_value(&body, "id").unwrap_or_default();
            let resp = match handle_get_task(&state, &serde_json::json!({ "id": id })) {
                Ok(v) => JsonRpcResponse::ok(None, v),
                Err(e) => JsonRpcResponse::err(None, -32000, e),
            };
            let b = serde_json::to_vec(&resp).unwrap_or_default();
            write_response(&mut stream, 200, "application/json", &b)?;
            return Ok(());
        }
    }

    // Helper: complete a task (specialist outbox shortcut via HTTP)
    if method == "POST" && path == "/v1/complete" {
        #[derive(serde::Deserialize)]
        struct CompleteBody {
            a2a_task_id: String,
            #[serde(default)]
            summary: String,
            #[serde(default)]
            failed: bool,
            #[serde(default)]
            error: Option<String>,
            #[serde(default)]
            rejected: bool,
            #[serde(default)]
            reason: Option<String>,
        }
        let parsed: CompleteBody = if is_form {
            CompleteBody {
                a2a_task_id: form_value(&body, "a2a_task_id").unwrap_or_default(),
                summary: form_value(&body, "summary").unwrap_or_default(),
                failed: form_value(&body, "failed").is_some_and(|v| v == "true" || v == "1"),
                error: form_value(&body, "error"),
                rejected: form_value(&body, "rejected").is_some_and(|v| v == "true" || v == "1"),
                reason: form_value(&body, "reason"),
            }
        } else {
            serde_json::from_slice(&body).unwrap_or(CompleteBody {
                a2a_task_id: String::new(),
                summary: String::new(),
                failed: false,
                error: None,
                rejected: false,
                reason: None,
            })
        };
        if parsed.a2a_task_id.is_empty() {
            write_response(&mut stream, 400, "text/plain", b"a2a_task_id required")?;
            return Ok(());
        }
        let ev = if parsed.rejected {
            BridgeEvent::Rejected {
                a2a_task_id: parsed.a2a_task_id,
                reason: parsed.reason.unwrap_or_else(|| "rejected".into()),
            }
        } else if parsed.failed {
            BridgeEvent::Failed {
                a2a_task_id: parsed.a2a_task_id,
                error: parsed.error.unwrap_or_else(|| "failed".into()),
            }
        } else {
            BridgeEvent::Completed {
                a2a_task_id: parsed.a2a_task_id,
                summary: parsed.summary,
            }
        };
        crate::FileBridge::write_completion(&state.config.a2a_dir, &ev)?;
        write_response(&mut stream, 200, "application/json", br#"{"ok":true}"#)?;
        return Ok(());
    }

    write_response(&mut stream, 404, "text/plain", b"not found")?;
    Ok(())
}

/// One field of an `application/x-www-form-urlencoded` body.
fn form_value(body: &[u8], key: &str) -> Option<String> {
    let body = std::str::from_utf8(body).ok()?;
    body.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        (percent_decode(k) == key).then(|| percent_decode(v))
    })
}

/// Form decoding: `+` is a space, `%XX` a byte; bytes are then read as UTF-8.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(byte) => {
                        out.push(byte);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn authorized(auth_header: &str, api_key: &str) -> bool {
    // No key configured means nobody is authorized, not everybody: the port
    // is reachable by every process (and every user) on the machine.
    if api_key.is_empty() {
        return false;
    }
    let token = auth_header
        .strip_prefix("Bearer ")
        .or_else(|| auth_header.strip_prefix("bearer "))
        .unwrap_or(auth_header.trim());
    constant_time_eq(token.as_bytes(), api_key.as_bytes())
}

/// Compare without stopping at the first differing byte, so response timing
/// does not reveal how much of a guessed key was right.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Extract tenant from `/t/<tenant>/.well-known/agent-card.json` (or `agent.json`).
fn tenant_from_card_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/t/")?;
    let tenant = rest
        .strip_suffix("/.well-known/agent-card.json")
        .or_else(|| rest.strip_suffix("/.well-known/agent.json"))?;
    if tenant.is_empty() {
        return None;
    }
    Some(crate::decode_tenant(tenant))
}

/// Extract tenant from `/t/<tenant>/jsonrpc`.
fn tenant_from_jsonrpc_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix("/t/")?;
    let tenant = rest.strip_suffix("/jsonrpc")?;
    if tenant.is_empty() {
        return None;
    }
    Some(crate::decode_tenant(tenant))
}

fn dispatch_rpc(
    state: &BrokerState,
    req: &JsonRpcRequest,
    tenant: Option<&str>,
) -> JsonRpcResponse {
    let id = req.id.clone();
    match req.method.as_str() {
        "message/send" | "SendMessage" => match handle_send_message(state, &req.params, tenant) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, -32000, e),
        },
        "tasks/get" | "GetTask" => match handle_get_task(state, &req.params) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, -32000, e),
        },
        "tasks/cancel" | "CancelTask" => match handle_cancel(state, &req.params) {
            Ok(v) => JsonRpcResponse::ok(id, v),
            Err(e) => JsonRpcResponse::err(id, -32000, e),
        },
        "tasks/list" | "ListTasks" => {
            let tasks = state.tasks.lock().ok();
            let list = tasks
                .map(|t| {
                    t.list_for_context(&state.config.oo_task_id)
                        .into_iter()
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            JsonRpcResponse::ok(id, serde_json::json!({ "tasks": list }))
        }
        "agent/getAuthenticatedExtendedCard" | "GetExtendedAgentCard" => {
            // Same as public card for v1.
            JsonRpcResponse::err(id, -32601, "use GET /.well-known/agent-card.json")
        }
        other => JsonRpcResponse::err(id, -32601, format!("method not found: {other}")),
    }
}

fn handle_send_message(
    state: &BrokerState,
    params: &serde_json::Value,
    tenant: Option<&str>,
) -> Result<serde_json::Value, String> {
    let params: SendMessageParams =
        serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
    let target = tenant
        .map(|s| s.to_string())
        .unwrap_or_else(|| state.config.coordinator_tenant.clone());

    if state.member(&target).is_none() {
        return Err(format!("unknown tenant {target}"));
    }

    let text = params
        .message
        .parts
        .iter()
        .filter_map(|p| p.as_text())
        .collect::<Vec<_>>()
        .join("\n");

    let a2a_task_id = params
        .message
        .task_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let context_id = params
        .message
        .context_id
        .clone()
        .unwrap_or_else(|| state.config.oo_task_id.clone());
    let ts = now_rfc3339();

    let mut history = vec![params.message.clone()];
    history[0].message_id = if history[0].message_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        history[0].message_id.clone()
    };
    history[0].context_id = Some(context_id.clone());
    history[0].task_id = Some(a2a_task_id.clone());
    history[0].role = Role::User;

    let task = Task {
        id: a2a_task_id.clone(),
        context_id: context_id.clone(),
        tenant: target.clone(),
        status: TaskStatus {
            state: TaskState::Submitted,
            message: None,
            timestamp: Some(ts.clone()),
        },
        history,
        artifacts: Vec::new(),
    };

    {
        let mut tasks = state.tasks.lock().map_err(|e| e.to_string())?;
        tasks.insert(task.clone());
        let _ = tasks.set_state(&a2a_task_id, TaskState::Working, None, ts.clone());
    }

    let inbox = InboxMessage {
        a2a_task_id: a2a_task_id.clone(),
        context_id: context_id.clone(),
        from_tenant: None,
        text: text.clone(),
        created_at: ts.clone(),
        reply: false,
    };
    state
        .bridge
        .enqueue(&target, &inbox)
        .map_err(|e| e.to_string())?;

    state.notify_inbox(InboxDelivery {
        oo_task_id: state.config.oo_task_id.clone(),
        tenant: target.clone(),
        message: inbox,
    });

    if let Ok(mut audit) = state.audit.lock() {
        let _ = audit.append(&AuditEvent {
            ts: &ts,
            kind: "message.send",
            oo_task_id: &state.config.oo_task_id,
            tenant: Some(&target),
            a2a_task_id: Some(&a2a_task_id),
            detail: Some(serde_json::json!({ "text": text })),
        });
    }

    state.notify_change();

    let tasks = state.tasks.lock().map_err(|e| e.to_string())?;
    let task = tasks.get(&a2a_task_id).cloned().ok_or("task missing")?;
    Ok(serde_json::to_value(task).unwrap_or_default())
}

fn handle_get_task(state: &BrokerState, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let id = params
        .get("id")
        .or_else(|| params.get("taskId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "id required".to_string())?;
    let tasks = state.tasks.lock().map_err(|e| e.to_string())?;
    let task = tasks.get(id).ok_or_else(|| format!("task {id} not found"))?;
    Ok(serde_json::to_value(task).unwrap_or_default())
}

fn handle_cancel(state: &BrokerState, params: &serde_json::Value) -> Result<serde_json::Value, String> {
    let id = params
        .get("id")
        .or_else(|| params.get("taskId"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "id required".to_string())?;
    let ts = now_rfc3339();
    let mut tasks = state.tasks.lock().map_err(|e| e.to_string())?;
    let task = tasks
        .cancel(id, ts)
        .ok_or_else(|| format!("task {id} not found"))?
        .clone();
    drop(tasks);
    state.notify_change();
    Ok(serde_json::to_value(task).unwrap_or_default())
}

fn handle_stream(
    stream: &mut TcpStream,
    state: &BrokerState,
    req: &JsonRpcRequest,
    _tenant: Option<&str>,
) -> std::io::Result<()> {
    // For message/stream: create task then stream status updates.
    let task_val = if req.method.contains("message") || req.method.contains("Message") {
        match handle_send_message(state, &req.params, _tenant) {
            Ok(v) => v,
            Err(e) => {
                let resp = JsonRpcResponse::err(req.id.clone(), -32000, e);
                let b = serde_json::to_vec(&resp).unwrap_or_default();
                return write_response(stream, 200, "application/json", &b);
            }
        }
    } else {
        // tasks/subscribe — params.id
        match handle_get_task(state, &req.params) {
            Ok(v) => v,
            Err(e) => {
                let resp = JsonRpcResponse::err(req.id.clone(), -32000, e);
                let b = serde_json::to_vec(&resp).unwrap_or_default();
                return write_response(stream, 200, "application/json", &b);
            }
        }
    };

    let task_id = task_val
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let header = concat!(
        "HTTP/1.1 200 OK\r\n",
        "Content-Type: text/event-stream\r\n",
        "Cache-Control: no-cache\r\n",
        "Connection: keep-alive\r\n",
        "\r\n"
    );
    stream.write_all(header.as_bytes())?;

    let submitted = JsonRpcResponse::ok(
        req.id.clone(),
        serde_json::json!({
            "taskId": task_id,
            "status": { "state": "submitted" }
        }),
    );
    write_sse(stream, &submitted)?;

    // Poll until terminal or timeout (~60s).
    for _ in 0..300 {
        drain_bridge(state);
        let state_name = {
            let tasks = state.tasks.lock().ok();
            tasks.and_then(|t| {
                t.get(&task_id).map(|task| {
                    serde_json::to_value(task.status.state)
                        .ok()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "working".into())
                })
            })
        };
        if let Some(lower) = state_name {
            let resp = JsonRpcResponse::ok(
                req.id.clone(),
                serde_json::json!({
                    "taskId": task_id,
                    "status": { "state": lower }
                }),
            );
            write_sse(stream, &resp)?;
            if matches!(
                lower.as_str(),
                "completed" | "failed" | "canceled" | "rejected"
            ) {
                break;
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

fn write_sse(stream: &mut TcpStream, resp: &JsonRpcResponse) -> std::io::Result<()> {
    let data = serde_json::to_string(resp).unwrap_or_default();
    write!(stream, "data: {data}\n\n")?;
    stream.flush()?;
    Ok(())
}

fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        _ => "Error",
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::FileBridge;
    use crate::{generate_api_key, BrokerConfig, MemberEndpoint};
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::time::Duration;

    pub(super) fn test_state(dir: &std::path::Path) -> BrokerState {
        test_state_with_notify(dir, None)
    }

    fn test_state_with_notify(
        dir: &std::path::Path,
        on_change: Option<crossbeam_channel::Sender<()>>,
    ) -> BrokerState {
        let a2a = dir.join("a2a");
        let bridge = Arc::new(FileBridge::new(&a2a).unwrap());
        let config = BrokerConfig {
            oo_task_id: "auth".into(),
            task_slug: "auth".into(),
            task_name: "Auth".into(),
            coordinator_tenant: crate::COORDINATOR_TENANT.into(),
            members: vec![
                MemberEndpoint {
                    worktree_id: crate::COORDINATOR_TENANT.into(),
                    repo: "coordinator".into(),
                    path: PathBuf::from("/tasks/auth"),
                    is_coordinator: true,
                },
                MemberEndpoint {
                    worktree_id: "jam/auth".into(),
                    repo: "jam".into(),
                    path: PathBuf::from("/wt/jam/auth"),
                    is_coordinator: false,
                },
                MemberEndpoint {
                    worktree_id: "api/auth".into(),
                    repo: "api".into(),
                    path: PathBuf::from("/wt/api/auth"),
                    is_coordinator: false,
                },
            ],
            api_key: "test-key".into(),
            a2a_dir: a2a,
            bind: "127.0.0.1:0".parse().unwrap(),
        };
        BrokerState::new_with_notify(config, bridge, on_change).unwrap()
    }

    #[test]
    fn send_message_enqueues_and_completes() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let handle = BrokerServer::start(state).unwrap();
        let base = handle.base_url();
        let key = handle.api_key().to_string();

        // Discover specialist card (encoded path — matches Agent Card URLs)
        let card_url = format!("{base}/t/api%2Fauth/.well-known/agent-card.json");
        let card = http_get(&card_url, &key);
        assert!(card.contains("repo-specialize"), "{card}");
        assert!(
            card.contains("api%2Fauth"),
            "card must advertise encoded tenant path: {card}"
        );

        // Literal-slash path still resolves (naive clients)
        let card_slash = http_get(
            &format!("{base}/t/api/auth/.well-known/agent-card.json"),
            &key,
        );
        assert!(card_slash.contains("repo-specialize"), "{card_slash}");

        let rpc = format!("{base}/t/api%2Fauth/jsonrpc");
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "message_id": "m1",
                    "role": "user",
                    "parts": [{ "text": "Add login endpoint" }]
                }
            }
        });
        let resp = http_post(&rpc, &key, &body.to_string());
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let task_id = v["result"]["id"].as_str().unwrap().to_string();
        assert_eq!(v["result"]["status"]["state"], "working");

        // Specialist completes via outbox
        FileBridge::write_completion(
            &handle.state.config.a2a_dir,
            &BridgeEvent::Completed {
                a2a_task_id: task_id.clone(),
                summary: "done".into(),
            },
        )
        .unwrap();

        thread::sleep(Duration::from_millis(500));
        let get_body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tasks/get",
            "params": { "id": task_id }
        });
        let got = http_post(&rpc, &key, &get_body.to_string());
        let gv: serde_json::Value = serde_json::from_str(&got).unwrap();
        assert_eq!(gv["result"]["status"]["state"], "completed");

        handle.shutdown();
    }

    fn send_body(text: &str) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": { "message": {
                "message_id": "m1", "role": "user", "parts": [{ "text": text }]
            } }
        })
        .to_string()
    }

    #[test]
    fn send_without_tenant_targets_the_coordinator() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let key = handle.api_key().to_string();
        let resp = http_post(
            &format!("{}/jsonrpc", handle.base_url()),
            &key,
            &send_body("plan this"),
        );
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let id = v["result"]["id"].as_str().unwrap().to_string();
        let tasks = handle.state.tasks.lock().unwrap();
        assert_eq!(tasks.get(&id).unwrap().tenant, "@coordinator");
        drop(tasks);
        handle.shutdown();
    }

    #[test]
    fn former_home_repo_is_a_delegation_target() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let key = handle.api_key().to_string();
        let resp = http_post(
            &format!("{}/t/jam%2Fauth/jsonrpc", handle.base_url()),
            &key,
            &send_body("do it"),
        );
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let id = v["result"]["id"].as_str().expect(&resp).to_string();
        let tasks = handle.state.tasks.lock().unwrap();
        assert_eq!(tasks.get(&id).unwrap().tenant, "jam/auth");
        drop(tasks);
        let inbox = handle.state.config.a2a_dir.join("inbox");
        assert!(
            std::fs::read_dir(&inbox)
                .unwrap()
                .flatten()
                .any(|e| e.file_name().to_string_lossy().contains("jam")),
            "expected an inbox entry for jam/auth under {inbox:?}"
        );
        handle.shutdown();
    }

    #[test]
    fn root_card_lists_every_repo_as_specialist() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let key = handle.api_key().to_string();
        let card = http_get(
            &format!("{}/.well-known/agent-card.json", handle.base_url()),
            &key,
        );
        let v: serde_json::Value = serde_json::from_str(&card).unwrap();
        let siblings: Vec<String> = v["metadata"]["openOrchestra"]["siblingCards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect();
        assert_eq!(siblings.len(), 2, "{siblings:?}");
        assert!(siblings.iter().any(|s| s.contains("jam%2Fauth")));
        assert!(siblings.iter().any(|s| s.contains("api%2Fauth")));
        assert!(!siblings.iter().any(|s| s.contains("coordinator")));
        assert_eq!(v["metadata"]["openOrchestra"]["worktreeId"], "@coordinator");
        handle.shutdown();
    }

    #[test]
    fn unknown_tenant_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let key = handle.api_key().to_string();
        let resp = http_post(
            &format!("{}/t/nope%2Fx/jsonrpc", handle.base_url()),
            &key,
            &send_body("hi"),
        );
        assert!(resp.contains("unknown tenant"), "{resp}");
        handle.shutdown();
    }

    #[test]
    fn cancel_task() {
        let dir = tempfile::tempdir().unwrap();
        let state = test_state(dir.path());
        let handle = BrokerServer::start(state).unwrap();
        let rpc = format!("{}/jsonrpc", handle.base_url());
        let key = handle.api_key().to_string();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "SendMessage",
            "params": {
                "message": {
                    "message_id": "m1",
                    "role": "user",
                    "parts": [{ "text": "x" }]
                }
            }
        });
        // POST to specialist tenant
        let rpc_t = format!("{}/t/api%2Fauth/jsonrpc", handle.base_url());
        let resp = http_post(&rpc_t, &key, &body.to_string());
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let task_id = v["result"]["id"].as_str().unwrap();
        let cancel = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tasks/cancel",
            "params": { "id": task_id }
        });
        let c = http_post(&rpc, &key, &cancel.to_string());
        let cv: serde_json::Value = serde_json::from_str(&c).unwrap();
        assert_eq!(cv["result"]["status"]["state"], "canceled");
        handle.shutdown();
    }

    fn http_get(url: &str, key: &str) -> String {
        raw_http("GET", url, key, None)
    }

    fn http_post(url: &str, key: &str, body: &str) -> String {
        raw_http("POST", url, key, Some(body))
    }

    fn raw_http(method: &str, url: &str, key: &str, body: Option<&str>) -> String {
        let url = url.strip_prefix("http://").unwrap();
        let (host_port, path) = url.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap();
        let addr: SocketAddr = host_port.parse().unwrap();
        let mut stream = TcpStream::connect(addr).unwrap();
        let body_bytes = body.unwrap_or("").as_bytes();
        let req = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host_port}\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body_bytes.len()
        );
        stream.write_all(req.as_bytes()).unwrap();
        if !body_bytes.is_empty() {
            stream.write_all(body_bytes).unwrap();
        }
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line == "\n" || line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
        let mut buf = vec![0u8; len];
        if len > 0 {
            reader.read_exact(&mut buf).unwrap();
        }
        String::from_utf8_lossy(&buf).into_owned()
    }

    #[test]
    fn generate_key_is_unique() {
        assert_ne!(generate_api_key(), generate_api_key());
    }

    #[test]
    fn tenant_path_parsers() {
        assert_eq!(
            tenant_from_card_path("/t/api%2Fauth/.well-known/agent-card.json").as_deref(),
            Some("api/auth")
        );
        assert_eq!(
            tenant_from_card_path("/t/api/auth/.well-known/agent-card.json").as_deref(),
            Some("api/auth")
        );
        assert_eq!(
            tenant_from_jsonrpc_path("/t/api%2Fauth/jsonrpc").as_deref(),
            Some("api/auth")
        );
        assert_eq!(
            tenant_from_jsonrpc_path("/t/api/auth/jsonrpc").as_deref(),
            Some("api/auth")
        );
        assert!(tenant_from_card_path("/t/.well-known/agent-card.json").is_none());
    }

    #[test]
    fn on_change_fires_on_send_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = crossbeam_channel::bounded(8);
        let state = test_state_with_notify(dir.path(), Some(tx));
        let handle = BrokerServer::start(state).unwrap();
        let rpc = format!("{}/t/api%2Fauth/jsonrpc", handle.base_url());
        let key = handle.api_key().to_string();
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "message/send",
            "params": {
                "message": {
                    "message_id": "m1",
                    "role": "user",
                    "parts": [{ "text": "hi" }]
                }
            }
        });
        let resp = http_post(&rpc, &key, &body.to_string());
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        let task_id = v["result"]["id"].as_str().unwrap().to_string();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());

        FileBridge::write_completion(
            &handle.state.config.a2a_dir,
            &BridgeEvent::Completed {
                a2a_task_id: task_id,
                summary: "ok".into(),
            },
        )
        .unwrap();
        assert!(rx.recv_timeout(Duration::from_secs(2)).is_ok());
        handle.shutdown();
    }

    /// Send a raw request head and return the status line.
    pub(super) fn status_of(addr: SocketAddr, head: &str) -> String {
        let mut stream = TcpStream::connect(addr).unwrap();
        stream.write_all(head.as_bytes()).unwrap();
        let mut status = String::new();
        BufReader::new(stream).read_line(&mut status).unwrap();
        status
    }

    #[test]
    fn a_huge_declared_body_is_refused_not_allocated() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let addr = handle.addr;
        // Without the key: refused before the body is considered at all.
        let st = status_of(
            addr,
            "POST /rpc HTTP/1.1\r\nContent-Length: 999999999999\r\n\r\n",
        );
        assert!(st.contains(" 401 "), "{st}");
        // With the key: refused for size, not allocated.
        let st = status_of(
            addr,
            "POST /rpc HTTP/1.1\r\nAuthorization: Bearer test-key\r\nContent-Length: 999999999999\r\n\r\n",
        );
        assert!(st.contains(" 413 "), "{st}");
        // A header line with no end is cut off rather than buffered forever.
        let long = format!("GET /health HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(20_000));
        let st = status_of(addr, &long);
        assert!(st.contains(" 431 "), "{st}");
        // And the server is still up.
        let st = status_of(addr, "GET /health HTTP/1.1\r\n\r\n");
        assert!(st.contains(" 200 "), "{st}");
        handle.shutdown();
    }

    #[test]
    fn drop_stops_threads_and_frees_port() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let addr = handle.addr;
        drop(handle);
        // Port should be reusable once accept/drain have joined.
        let listener = TcpListener::bind(addr).expect("port freed after Drop");
        drop(listener);
    }
}

#[cfg(test)]
mod coordinator_push_tests {
    use super::*;
    use super::tests::test_state;

    fn start() -> (
        tempfile::TempDir,
        BrokerHandle,
        crossbeam_channel::Receiver<InboxDelivery>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut state = test_state(dir.path());
        let (tx, rx) = crossbeam_channel::unbounded();
        state.on_inbox = Some(tx);
        let handle = BrokerServer::start(state).unwrap();
        (dir, handle, rx)
    }

    fn form(handle: &BrokerHandle, path: &str, body: &str) -> serde_json::Value {
        let mut s = TcpStream::connect(handle.addr).unwrap();
        write!(
            s,
            "POST {path} HTTP/1.1\r\nAuthorization: Bearer test-key\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let json = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        serde_json::from_str(&json).unwrap_or(serde_json::Value::Null)
    }

    /// Send to api/auth; returns the delegated task id and drops the
    /// specialist's own delivery.
    fn delegate(
        handle: &BrokerHandle,
        rx: &crossbeam_channel::Receiver<InboxDelivery>,
    ) -> (String, InboxDelivery) {
        let sent = form(handle, "/t/api%2Fauth/send", "text=do+it");
        let id = sent["result"]["id"].as_str().unwrap().to_string();
        let d = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        (id, d)
    }

    fn coordinator_reply(
        rx: &crossbeam_channel::Receiver<InboxDelivery>,
    ) -> InboxDelivery {
        rx.recv_timeout(Duration::from_secs(2)).expect("reply pushed")
    }

    #[test]
    fn completion_pushes_one_reply_to_coordinator() {
        let (dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        form(&handle, "/v1/complete", &format!("a2a_task_id={id}&summary=shipped"));
        let d = coordinator_reply(&rx);
        assert_eq!(d.tenant, "@coordinator");
        assert!(d.message.reply);
        assert_eq!(d.message.text, "completed by api: shipped");
        assert_eq!(d.message.a2a_task_id, id);
        assert_eq!(d.message.from_tenant.as_deref(), Some("api/auth"));
        thread::sleep(Duration::from_millis(500));
        assert!(rx.try_recv().is_err(), "exactly one delivery");
        let file = dir.path().join("a2a/inbox/@coordinator.jsonl");
        assert_eq!(std::fs::read_to_string(file).unwrap().lines().count(), 1);
        handle.shutdown();
    }

    #[test]
    fn failure_pushes_failed_reply() {
        let (_dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        form(&handle, "/v1/complete", &format!("a2a_task_id={id}&failed=1&error=boom"));
        assert_eq!(coordinator_reply(&rx).message.text, "failed by api: boom");
        handle.shutdown();
    }

    #[test]
    fn rejection_pushes_rejected_reply() {
        let (_dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        form(
            &handle,
            "/v1/complete",
            &format!("a2a_task_id={id}&rejected=1&reason=not+my+repo"),
        );
        assert_eq!(coordinator_reply(&rx).message.text, "rejected by api: not my repo");
        assert_eq!(
            handle.state.tasks.lock().unwrap().get(&id).unwrap().status.state,
            TaskState::Rejected
        );
        handle.shutdown();
    }

    #[test]
    fn repeated_complete_does_not_push_twice() {
        let (_dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        let body = format!("a2a_task_id={id}&summary=shipped");
        form(&handle, "/v1/complete", &body);
        coordinator_reply(&rx);
        form(&handle, "/v1/complete", &body);
        thread::sleep(Duration::from_millis(700));
        assert!(rx.try_recv().is_err(), "second complete must not push");
        handle.shutdown();
    }

    fn json_post(handle: &BrokerHandle, path: &str, body: &str) -> String {
        let mut s = TcpStream::connect(handle.addr).unwrap();
        write!(
            s,
            "POST {path} HTTP/1.1\r\nAuthorization: Bearer test-key\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn state_of(handle: &BrokerHandle, id: &str) -> TaskState {
        handle.state.tasks.lock().unwrap().get(id).unwrap().status.state
    }

    #[test]
    fn terminal_state_is_final() {
        let (_dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        form(&handle, "/v1/complete", &format!("a2a_task_id={id}&summary=shipped"));
        coordinator_reply(&rx);
        assert_eq!(state_of(&handle, &id), TaskState::Completed);
        form(
            &handle,
            "/v1/complete",
            &format!("a2a_task_id={id}&rejected=1&reason=late"),
        );
        thread::sleep(Duration::from_millis(700));
        assert_eq!(state_of(&handle, &id), TaskState::Completed, "a late reject must not flip it");
        let tasks = handle.state.tasks.lock().unwrap();
        assert_eq!(tasks.get(&id).unwrap().artifacts.len(), 1, "one summary artifact");
        drop(tasks);
        // A repeated complete must not duplicate the summary artifact either.
        form(&handle, "/v1/complete", &format!("a2a_task_id={id}&summary=again"));
        thread::sleep(Duration::from_millis(700));
        let tasks = handle.state.tasks.lock().unwrap();
        assert_eq!(tasks.get(&id).unwrap().artifacts.len(), 1, "no duplicate artifact");
        drop(tasks);
        assert!(rx.try_recv().is_err(), "no second push");
        handle.shutdown();
    }

    #[test]
    fn json_rejected_body_rejects() {
        let (_dir, handle, rx) = start();
        let (id, _) = delegate(&handle, &rx);
        let body = serde_json::json!({ "a2a_task_id": id, "rejected": true, "reason": "not mine" })
            .to_string();
        let out = json_post(&handle, "/v1/complete", &body);
        assert!(out.starts_with("HTTP/1.1 200"), "{out}");
        assert_eq!(coordinator_reply(&rx).message.text, "rejected by api: not mine");
        assert_eq!(state_of(&handle, &id), TaskState::Rejected);
        handle.shutdown();
    }

    #[test]
    fn coordinator_completing_its_own_inbox_is_not_echoed() {
        let (_dir, handle, rx) = start();
        // A specialist messages the coordinator.
        let sent = form(&handle, "/t/@coordinator/send", "text=question");
        let id = sent["result"]["id"].as_str().unwrap().to_string();
        let d = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(d.tenant, "@coordinator");
        // The coordinator completes it: no reply is pushed back to itself.
        form(&handle, "/v1/complete", &format!("a2a_task_id={id}&summary=answered"));
        thread::sleep(Duration::from_millis(700));
        assert_eq!(state_of(&handle, &id), TaskState::Completed);
        assert!(rx.try_recv().is_err(), "no echo to the coordinator");
        handle.shutdown();
    }

    #[test]
    fn delegation_inbox_is_not_a_reply() {
        let (_dir, handle, rx) = start();
        let (_, d) = delegate(&handle, &rx);
        assert_eq!(d.tenant, "api/auth");
        assert!(!d.message.reply);
        handle.shutdown();
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;
    use super::tests::{status_of, test_state};

    #[test]
    fn timestamps_are_rfc3339() {
        assert_eq!(rfc3339_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339_utc(1_794_614_400 + 3_723), "2026-11-14T01:02:03Z");
    }

    #[test]
    fn form_values_decode() {
        let body = b"text=hello+world%21&id=a%2Fb&empty=&bad=%zz";
        assert_eq!(form_value(body, "text").as_deref(), Some("hello world!"));
        assert_eq!(form_value(body, "id").as_deref(), Some("a/b"));
        assert_eq!(form_value(body, "empty").as_deref(), Some(""));
        assert_eq!(form_value(body, "bad").as_deref(), Some("%zz"));
        assert_eq!(form_value(body, "missing"), None);
        assert_eq!(form_value("x=caf%C3%A9".as_bytes(), "x").as_deref(), Some("café"));
    }

    #[test]
    fn only_the_bare_health_path_skips_auth() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        assert!(status_of(handle.addr, "GET /health HTTP/1.1\r\n\r\n").contains("200"));
        assert!(status_of(handle.addr, "GET /t/x/health HTTP/1.1\r\n\r\n").contains("401"));
        handle.shutdown();
    }

    #[test]
    fn the_form_helpers_send_check_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let handle = BrokerServer::start(test_state(dir.path())).unwrap();
        let form = |path: &str, body: &str| {
            let mut s = TcpStream::connect(handle.addr).unwrap();
            write!(
                s,
                "POST {path} HTTP/1.1\r\nAuthorization: Bearer test-key\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            let mut out = String::new();
            s.read_to_string(&mut out).unwrap();
            let json = out.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            serde_json::from_str::<serde_json::Value>(&json).unwrap()
        };
        let sent = form("/t/api%2Fauth/send", "text=Add+a+%22login%22+endpoint");
        let id = sent["result"]["id"].as_str().unwrap().to_string();
        assert_eq!(sent["result"]["history"][0]["parts"][0]["text"], "Add a \"login\" endpoint");

        form("/v1/complete", &format!("a2a_task_id={id}&summary=shipped"));
        thread::sleep(Duration::from_millis(500));
        let got = form("/v1/status", &format!("id={id}"));
        assert_eq!(got["result"]["status"]["state"], "completed");
        handle.shutdown();
    }

    #[test]
    fn an_empty_key_authorizes_nobody() {
        assert!(!authorized("", ""));
        assert!(!authorized("Bearer anything", ""));
    }

    #[test]
    fn the_key_must_match_exactly() {
        assert!(authorized("Bearer oo-abc", "oo-abc"));
        assert!(!authorized("Bearer oo-abd", "oo-abc"));
        assert!(!authorized("Bearer oo-ab", "oo-abc"));
        assert!(constant_time_eq(b"x", b"x"));
        assert!(!constant_time_eq(b"x", b"xy"));
    }
}
