//! HTTPS routes: peer filter, bearer auth, pairing, the agents list and SSE,
//! quick-reply send. The terminal socket lives in `ws.rs`, static files in
//! `assets.rs`.
//!
//! Request paths always re-read `remote.json` (one small file) so marks,
//! revocations and read-only flags apply immediately; the watcher only
//! drives pushes to already-open streams.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use futures_util::stream::{self, Stream};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, PairError};
use crate::netfilter::is_allowed_peer;
use crate::server::Shared;
use crate::{Device, RemoteFile};

/// Max quick-reply text (bytes).
pub(crate) const MAX_SEND_TEXT: usize = 16 * 1024;
/// Request bodies above this are rejected (413) before parsing.
const MAX_BODY: usize = 64 * 1024;
const MAX_DEVICE_NAME: usize = 64;
const MAX_CODE: usize = 32;
/// Upper bound on opening a session's control client.
pub(crate) const HUB_OPEN_TIMEOUT: Duration = Duration::from_secs(5);
const SSE_KEEPALIVE: Duration = Duration::from_secs(15);

pub(crate) type AppState = State<Arc<Shared>>;

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/api/pair", post(pair))
        .route("/api/me", get(me))
        .route("/api/agents", get(agents))
        .route("/api/events", get(events))
        .route("/api/agents/{id}/send", post(send))
        .route("/api/term/{id}", get(crate::ws::term))
        .route("/api", any(not_found))
        .route("/api/{*rest}", any(not_found))
        .fallback(crate::assets::serve)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        // Outermost: every route, including the fallback.
        .layer(middleware::from_fn(peer_filter))
        .with_state(shared)
}

/// 403 for peers outside loopback / RFC 1918 / link-local / ULA.
pub(crate) async fn peer_filter(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !is_allowed_peer(peer.ip()) {
        return error(StatusCode::FORBIDDEN, "forbidden");
    }
    next.run(req).await
}

pub(crate) fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim())
        .filter(|t| !t.is_empty())
}

/// Authenticate `token` against the current file; records `last_seen` in
/// the background (self-throttled).
pub(crate) fn authenticate(s: &Arc<Shared>, token: &str) -> Option<(Device, RemoteFile)> {
    let file = s.current_file();
    let device = auth::authenticate(&file, token)?.clone();
    let (s2, id) = (s.clone(), device.id.clone());
    tokio::task::spawn_blocking(move || {
        let _ = auth::touch_last_seen(&s2.store(), &id, SystemTime::now());
    });
    Some((device, file))
}

#[allow(clippy::result_large_err)]
fn authed(s: &Arc<Shared>, headers: &HeaderMap) -> Result<(Device, RemoteFile), Response> {
    bearer(headers)
        .and_then(|t| authenticate(s, t))
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))
}

/// Request host without the port (`[v6]` keeps its brackets).
fn host_without_port(headers: &HeaderMap, uri: &Uri) -> String {
    let raw = headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(str::to_string)
        .or_else(|| uri.authority().map(|a| a.as_str().to_string()))
        .unwrap_or_default();
    strip_port(&raw).to_ascii_lowercase()
}

pub(crate) fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        return match host.find(']') {
            Some(i) => &host[..=i],
            None => host,
        };
    }
    match host.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.bytes().all(|b| b.is_ascii_digit()) => h,
        _ => host,
    }
}

#[derive(Deserialize)]
struct PairReq {
    code: String,
    device_name: String,
}

async fn pair(
    State(s): AppState,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Response {
    // Counted before anything else: the global lockout in `auth` relies on it.
    if !s.pair_attempt_allowed(peer.ip(), Instant::now()) {
        return error(StatusCode::TOO_MANY_REQUESTS, "rate_limited");
    }
    let Ok(req) = serde_json::from_slice::<PairReq>(&body) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    if req.code.len() > MAX_CODE {
        return error(StatusCode::UNAUTHORIZED, "invalid");
    }
    let name: String = req
        .device_name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_DEVICE_NAME)
        .collect();
    let name = match name.trim() {
        "" => "Phone".to_string(),
        n => n.to_string(),
    };
    let origin_host = host_without_port(&headers, &uri);
    let s2 = s.clone();
    let result = tokio::task::spawn_blocking(move || {
        auth::redeem_code(
            &s2.store(),
            &req.code,
            &name,
            &origin_host,
            SystemTime::now(),
        )
    })
    .await;
    match result {
        Ok(Ok((_, token))) => Json(json!({ "token": token })).into_response(),
        Ok(Err(PairError::Invalid)) => error(StatusCode::UNAUTHORIZED, "invalid"),
        Ok(Err(PairError::Locked)) => error(StatusCode::LOCKED, "locked"),
        Ok(Err(PairError::Io(e))) => {
            eprintln!("oo-remote: pairing failed: {e}");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

async fn me(State(s): AppState, headers: HeaderMap) -> Response {
    match authed(&s, &headers) {
        Ok((d, _)) => Json(json!({ "name": d.name, "read_only": d.read_only })).into_response(),
        Err(r) => r,
    }
}

async fn agents(State(s): AppState, headers: HeaderMap) -> Response {
    match authed(&s, &headers) {
        Ok((_, file)) => Json(s.agents_for(&file)).into_response(),
        Err(r) => r,
    }
}

/// SSE: `event: agents` with the full list on connect and on every change.
/// Ends with `event: closed` (`revoked` / `disabled`) when access ends.
async fn events(State(s): AppState, headers: HeaderMap) -> Response {
    let (device, file) = match authed(&s, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let first = s.agents_for(&file);
    Sse::new(agent_events(s, device.id, first))
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
        .into_response()
}

fn agents_event(list: &[crate::server::AgentInfo]) -> Event {
    Event::default()
        .event("agents")
        .data(serde_json::to_string(list).unwrap_or_else(|_| "[]".into()))
}

fn closed_event(reason: &str) -> Event {
    Event::default()
        .event("closed")
        .data(json!({ "reason": reason }).to_string())
}

enum SseState {
    First(Vec<crate::server::AgentInfo>),
    Live,
    Done,
}

fn agent_events(
    s: Arc<Shared>,
    device_id: String,
    first: Vec<crate::server::AgentInfo>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    struct St {
        s: Arc<Shared>,
        device_id: String,
        stop: tokio::sync::watch::Receiver<bool>,
        file: tokio::sync::watch::Receiver<Arc<RemoteFile>>,
        agents: tokio::sync::watch::Receiver<Arc<Vec<crate::server::AgentInfo>>>,
        phase: SseState,
        _open: Arc<()>,
    }
    let mut agents = s.agents.subscribe();
    agents.mark_unchanged();
    let st = St {
        stop: s.shutdown.subscribe(),
        file: s.file.subscribe(),
        agents,
        _open: s.open_streams.clone(),
        device_id,
        phase: SseState::First(first),
        s,
    };
    stream::unfold(st, |mut st| async move {
        match std::mem::replace(&mut st.phase, SseState::Live) {
            SseState::Done => return None,
            SseState::First(list) => return Some((Ok(agents_event(&list)), st)),
            SseState::Live => {}
        }
        enum Wake {
            Stop,
            File(bool),
            Agents(bool),
        }
        loop {
            let wake = tokio::select! {
                biased;
                _ = crate::server::stopped(&mut st.stop) => Wake::Stop,
                r = st.file.changed() => Wake::File(r.is_ok()),
                r = st.agents.changed() => Wake::Agents(r.is_ok()),
            };
            match wake {
                Wake::Stop => {
                    st.phase = SseState::Done;
                    return Some((Ok(closed_event("disabled")), st));
                }
                Wake::File(false) | Wake::Agents(false) => return None,
                Wake::File(true) => {
                    let gone = !st
                        .file
                        .borrow_and_update()
                        .devices
                        .iter()
                        .any(|d| d.id == st.device_id);
                    if gone {
                        st.phase = SseState::Done;
                        return Some((Ok(closed_event("revoked")), st));
                    }
                }
                Wake::Agents(true) => {
                    // Re-filter against the current file: the shared list
                    // may lag an unmark by one watcher tick.
                    let file = st.s.current_file();
                    let list = st.s.agents_for(&file);
                    return Some((Ok(agents_event(&list)), st));
                }
            }
        }
    })
}

#[derive(Deserialize)]
struct SendReq {
    text: String,
}

/// `\r\n`, `\r` and `\n` each become one space.
pub fn flatten_newlines(text: &str) -> String {
    text.replace("\r\n", " ").replace(['\r', '\n'], " ")
}

/// Quick reply: 401, 403 read-only, 413 too long, 404 unknown/unmarked,
/// 409 stopped, 503 when the control client can't be opened in time.
async fn send(
    State(s): AppState,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let (device, file) = match authed(&s, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if device.read_only {
        return error(StatusCode::FORBIDDEN, "read_only");
    }
    let Ok(req) = serde_json::from_slice::<SendReq>(&body) else {
        return error(StatusCode::BAD_REQUEST, "bad_request");
    };
    if req.text.len() > MAX_SEND_TEXT {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    let Some(session) = s.session_for(&file, &id) else {
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    if !crate::tmux_bridge::live_sessions(&s.tmux)
        .await
        .contains(&session)
    {
        return error(StatusCode::CONFLICT, "stopped");
    }
    let hub = match tokio::time::timeout(HUB_OPEN_TIMEOUT, s.hubs.get(&session)).await {
        Ok(Ok(hub)) => hub,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            return error(StatusCode::CONFLICT, "stopped")
        }
        Ok(Err(e)) => {
            eprintln!("oo-remote: open session failed: {e}");
            return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        }
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    let mut bytes = flatten_newlines(&req.text).into_bytes();
    bytes.push(b'\r');
    match hub.send_input(&bytes).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            eprintln!("oo-remote: send failed: {e}");
            error(StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt;

    #[test]
    fn flattens_each_newline_kind_to_one_space() {
        assert_eq!(flatten_newlines("a\nb"), "a b");
        assert_eq!(flatten_newlines("a\r\nb\rc"), "a b c");
        assert_eq!(flatten_newlines("a\n\nb"), "a  b");
    }

    #[test]
    fn strips_ports() {
        assert_eq!(strip_port("mac.local:7443"), "mac.local");
        assert_eq!(strip_port("192.168.1.2:7443"), "192.168.1.2");
        assert_eq!(strip_port("[fe80::1]:7443"), "[fe80::1]");
        assert_eq!(strip_port("mac.local"), "mac.local");
        assert_eq!(strip_port("fe80::1"), "fe80::1");
    }

    #[test]
    fn bearer_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, "Bearer abc".parse().unwrap());
        assert_eq!(bearer(&h), Some("abc"));
        h.insert(header::AUTHORIZATION, "bearer  xyz ".parse().unwrap());
        assert_eq!(bearer(&h), Some("xyz"));
        h.insert(header::AUTHORIZATION, "Basic abc".parse().unwrap());
        assert_eq!(bearer(&h), None);
        h.insert(header::AUTHORIZATION, "Bearer ".parse().unwrap());
        assert_eq!(bearer(&h), None);
    }

    #[tokio::test]
    async fn peer_filter_rejects_public_ip_on_any_path() {
        let app = Router::new()
            .fallback(|| async { "ok" })
            .layer(middleware::from_fn(peer_filter));
        for (ip, want) in [
            ("8.8.8.8:1234", StatusCode::FORBIDDEN),
            ("192.168.1.9:1234", StatusCode::OK),
        ] {
            let mut req = Request::builder()
                .uri("/whatever")
                .body(Body::empty())
                .unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
            let resp = app.clone().oneshot(req).await.unwrap();
            assert_eq!(resp.status(), want, "{ip}");
        }
    }
}
