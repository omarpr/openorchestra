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
    format!("{secs}")
}

fn drain_bridge(state: &BrokerState) {
    while let Some(ev) = state.bridge.try_recv_completion() {
        let ts = now_rfc3339();
        let mut tasks = match state.tasks.lock() {
            Ok(t) => t,
            Err(_) => return,
        };
        match ev {
            BridgeEvent::Completed {
                a2a_task_id,
                summary,
            } => {
                let artifact = Artifact {
                    artifact_id: uuid::Uuid::new_v4().to_string(),
                    name: Some("summary".into()),
                    parts: vec![MessagePart::text(summary.clone())],
                };
                if let Some(task) = tasks.get_mut(&a2a_task_id) {
                    task.artifacts.push(artifact);
                }
                let _ = tasks.set_state(
                    &a2a_task_id,
                    TaskState::Completed,
                    Some(Message {
                        message_id: uuid::Uuid::new_v4().to_string(),
                        context_id: Some(state.config.oo_task_id.clone()),
                        task_id: Some(a2a_task_id.clone()),
                        role: Role::Agent,
                        parts: vec![MessagePart::text(summary)],
                    }),
                    ts.clone(),
                );
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
                state.notify_change();
            }
            BridgeEvent::Failed { a2a_task_id, error } => {
                let _ = tasks.set_state(
                    &a2a_task_id,
                    TaskState::Failed,
                    Some(Message {
                        message_id: uuid::Uuid::new_v4().to_string(),
                        context_id: Some(state.config.oo_task_id.clone()),
                        task_id: Some(a2a_task_id.clone()),
                        role: Role::Agent,
                        parts: vec![MessagePart::text(error)],
                    }),
                    ts,
                );
                state.notify_change();
            }
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

    if !authorized(&auth, &state.config.api_key) && !path.ends_with("/health") {
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

    // Root card = orchestrator
    if method == "GET" && (path == "/.well-known/agent-card.json" || path == "/.well-known/agent.json")
    {
        let home = state
            .config
            .members
            .iter()
            .find(|m| m.is_home)
            .or_else(|| state.config.members.first());
        let Some(home) = home else {
            write_response(&mut stream, 404, "text/plain", b"no home")?;
            return Ok(());
        };
        let siblings: Vec<_> = state
            .config
            .members
            .iter()
            .filter(|m| !m.is_home)
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
        }
        let parsed: CompleteBody = serde_json::from_slice(&body).unwrap_or(CompleteBody {
            a2a_task_id: String::new(),
            summary: String::new(),
            failed: false,
            error: None,
        });
        if parsed.a2a_task_id.is_empty() {
            write_response(&mut stream, 400, "text/plain", b"a2a_task_id required")?;
            return Ok(());
        }
        let ev = if parsed.failed {
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
        .unwrap_or_else(|| state.config.home_worktree_id.clone());

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

    fn test_state(dir: &std::path::Path) -> BrokerState {
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
            home_worktree_id: "jam/auth".into(),
            home_repo: "jam".into(),
            members: vec![
                MemberEndpoint {
                    worktree_id: "jam/auth".into(),
                    repo: "jam".into(),
                    path: PathBuf::from("/wt/jam/auth"),
                    is_home: true,
                },
                MemberEndpoint {
                    worktree_id: "api/auth".into(),
                    repo: "api".into(),
                    path: PathBuf::from("/wt/api/auth"),
                    is_home: false,
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
    fn status_of(addr: SocketAddr, head: &str) -> String {
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
mod hardening_tests {
    use super::{authorized, constant_time_eq};

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
