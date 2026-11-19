//! HTTPS routes: peer filter, bearer auth, pairing, the agents list and SSE,
//! quick-reply send. The terminal socket lives in `ws.rs`, the conversation
//! stream in `conversation_api.rs`, static files in `assets.rs`.
//!
//! Request paths always re-read `remote.json` (one small file) so marks,
//! revocations and read-only flags apply immediately; the watcher only
//! drives pushes to already-open streams.

use std::convert::Infallible;
use std::io;
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
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
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
const MAX_SCREENSHOT_BYTES: usize = 8 * 1024 * 1024;
const MAX_ENCODED_SCREENSHOT: usize = MAX_SCREENSHOT_BYTES.div_ceil(3) * 4;
// Base64 for the image, room for JSON escaping of the caption and the wrapper.
const MAX_SCREENSHOT_BODY: usize = MAX_ENCODED_SCREENSHOT + MAX_SEND_TEXT * 6 + 128;
const MAX_SCREENSHOT_DIMENSION: u32 = 16_384;
const MAX_SCREENSHOT_DECODED_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DEVICE_NAME: usize = 64;
const MAX_CODE: usize = 32;
/// Upper bound on opening a session's control client.
pub(crate) const HUB_OPEN_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const SSE_KEEPALIVE: Duration = Duration::from_secs(15);
pub(crate) const MAX_CONCURRENT_SCREENSHOTS: usize = 2;
pub(crate) const SCREENSHOT_READ_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) type AppState = State<Arc<Shared>>;

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/api/pair", post(pair))
        .route("/api/me", get(me))
        .route("/api/voice", post(crate::voice::transcribe))
        .route("/api/agents", get(agents))
        .route("/api/events", get(events))
        .route("/api/agents/{id}/send", post(send))
        .route("/api/agents/{id}/send-screenshot", post(send_screenshot))
        .route(
            "/api/agents/{id}/conversation",
            get(crate::conversation_api::stream),
        )
        .route(
            "/api/agents/{id}/conversation/tool/{tool_id}",
            get(crate::conversation_api::tool),
        )
        .route("/api/term/{id}", get(crate::ws::term))
        .route("/api", any(not_found))
        .route("/api/{*rest}", any(not_found))
        .fallback(crate::assets::serve)
        .layer(DefaultBodyLimit::max(MAX_BODY))
        // Outermost: every route, including the fallback.
        .layer(middleware::from_fn_with_state(
            shared.clone(),
            trusted_filter,
        ))
        .layer(middleware::from_fn(peer_filter))
        .layer(middleware::from_fn(security_headers))
        .with_state(shared)
}

/// Headers every response carries, errors and JSON included (`assets.rs`
/// adds the CSP to HTML). `insert`, so a route's own value is replaced by
/// the same one rather than doubled. No HSTS: it binds the whole host, and
/// the plain-HTTP setup page on port 7080 shares it.
pub(crate) async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    resp
}

/// 403 `untrusted_network` for a peer that isn't on a network the user
/// trusted (see `crate::network`). Inside `peer_filter`.
pub(crate) async fn trusted_filter(
    State(s): AppState,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if !s.peer_trusted(peer.ip()) {
        return error(StatusCode::FORBIDDEN, "untrusted_network");
    }
    next.run(req).await
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
pub(crate) fn authed(
    s: &Arc<Shared>,
    headers: &HeaderMap,
) -> Result<(Device, RemoteFile), Response> {
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
        "" => "Device".to_string(),
        n => n.to_string(),
    };
    if s.pair_locked(peer.ip(), Instant::now()) {
        return error(StatusCode::LOCKED, "address_locked");
    }
    let origin_host = host_without_port(&headers, &uri);
    let logged_name = name.clone();
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
        // Pairing outcomes go to the app log, so a guessing attempt leaves a
        // trace (`{:?}` keeps a device-chosen name on one quoted line).
        Ok(Ok((_, token))) => {
            s.pair_result(peer.ip(), true, Instant::now());
            eprintln!("oo-remote: paired {logged_name:?} from {}", peer.ip());
            Json(json!({ "token": token })).into_response()
        }
        Ok(Err(PairError::Invalid)) => {
            s.pair_result(peer.ip(), false, Instant::now());
            eprintln!("oo-remote: wrong pairing code from {}", peer.ip());
            if s.pair_locked(peer.ip(), Instant::now()) {
                eprintln!(
                    "oo-remote: {} locked out of pairing after repeated wrong codes",
                    peer.ip()
                );
            }
            error(StatusCode::UNAUTHORIZED, "invalid")
        }
        Ok(Err(PairError::Locked)) => {
            eprintln!(
                "oo-remote: pairing codes voided after too many wrong codes (last from {})",
                peer.ip()
            );
            error(StatusCode::LOCKED, "locked")
        }
        Ok(Err(PairError::Io(e))) => {
            eprintln!("oo-remote: pairing failed: {e}");
            error(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
        Err(_) => error(StatusCode::INTERNAL_SERVER_ERROR, "internal"),
    }
}

async fn me(State(s): AppState, headers: HeaderMap) -> Response {
    match authed(&s, &headers) {
        Ok((d, _)) => {
            let mut reply = json!({ "name": d.name, "read_only": d.read_only });
            if !d.read_only && s.voice.available().await {
                reply["voice_input"] = json!(true);
            }
            Json(reply).into_response()
        }
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
    // Subscribe before authenticating so a revoke racing the request is
    // still seen as a change.
    let rx = Receivers {
        stop: s.shutdown.subscribe(),
        file: s.file.subscribe(),
        agents: s.agents.subscribe(),
    };
    let (device, file) = match authed(&s, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let first = s.agents_for(&file);
    Sse::new(agent_events(s, rx, device.id, first))
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
        .into_response()
}

fn agents_event(list: &[crate::server::AgentInfo]) -> Event {
    Event::default()
        .event("agents")
        .data(serde_json::to_string(list).unwrap_or_else(|_| "[]".into()))
}

pub(crate) fn closed_event(reason: &str) -> Event {
    Event::default()
        .event("closed")
        .data(json!({ "reason": reason }).to_string())
}

struct Receivers {
    stop: tokio::sync::watch::Receiver<bool>,
    file: tokio::sync::watch::Receiver<Arc<RemoteFile>>,
    agents: tokio::sync::watch::Receiver<Arc<Vec<crate::server::AgentInfo>>>,
}

enum SseState {
    First(Vec<crate::server::AgentInfo>),
    Live,
    Done,
}

fn agent_events(
    s: Arc<Shared>,
    rx: Receivers,
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
    let st = St {
        stop: rx.stop,
        file: rx.file,
        agents: rx.agents,
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

/// Fresh re-check: `id` still maps to `session` and the device is still
/// paired and writable.
pub(crate) fn still_allowed(s: &Shared, device_id: &str, id: &str, session: &str) -> bool {
    let file = s.current_file();
    file.devices
        .iter()
        .any(|d| d.id == device_id && !d.read_only)
        && s.session_for(&file, id).as_deref() == Some(session)
}

/// Fresh re-check for screenshots: `id` still maps to `session` with the same
/// `kind`, and the device is still paired and writable. Returns `Some(true)`
/// for native Codex image delivery when allowed.
fn screenshot_still_allowed(
    s: &Shared,
    device_id: &str,
    id: &str,
    session: &str,
    kind: &str,
) -> Option<bool> {
    let file = s.current_file();
    if !file
        .devices
        .iter()
        .any(|d| d.id == device_id && !d.read_only)
    {
        return None;
    }
    let (cur_session, cur_kind) = s.screenshot_target(&file, id)?;
    (cur_session == session && cur_kind == kind).then(|| cur_kind == "codex")
}

#[derive(Deserialize)]
struct SendReq {
    text: String,
}

#[derive(Deserialize)]
struct ScreenshotReq<'a> {
    text: String,
    image: &'a str,
}

struct Screenshot {
    bytes: Vec<u8>,
    extension: &'static str,
    kind: String,
}

fn screenshot_extension(bytes: &[u8]) -> Option<&'static str> {
    if validate_png(bytes) {
        Some("png")
    } else if validate_jpeg(bytes) {
        Some("jpg")
    } else if validate_webp(bytes) {
        Some("webp")
    } else {
        None
    }
}

fn screenshot_dimensions_ok(width: u32, height: u32, bytes_per_pixel: u64) -> bool {
    width > 0
        && height > 0
        && width <= MAX_SCREENSHOT_DIMENSION
        && height <= MAX_SCREENSHOT_DIMENSION
        && (width as u64)
            .checked_mul(height as u64)
            .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
            .is_some_and(|bytes| bytes <= MAX_SCREENSHOT_DECODED_BYTES)
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

fn validate_png(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let mut pos = 8usize;
    let mut saw_ihdr = false;
    let mut saw_idat = false;
    let mut saw_iend = false;
    while let Some(header) = bytes.get(pos..pos + 8) {
        let len = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
        let ctype = &header[4..8];
        let Some(data_end) = pos.checked_add(8).and_then(|n| n.checked_add(len)) else {
            return false;
        };
        let Some(chunk_end) = data_end.checked_add(4) else {
            return false;
        };
        let Some(crc_bytes) = bytes.get(data_end..chunk_end) else {
            return false;
        };
        let expected_crc =
            u32::from_be_bytes([crc_bytes[0], crc_bytes[1], crc_bytes[2], crc_bytes[3]]);
        if crc32_ieee(&bytes[pos + 4..data_end]) != expected_crc {
            return false;
        }
        let data = &bytes[pos + 8..data_end];
        if !saw_ihdr {
            if ctype != b"IHDR" || len != 13 {
                return false;
            }
            let width = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
            let height = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
            let bit_depth = data[8];
            let color_type = data[9];
            let valid_format = match color_type {
                0 => matches!(bit_depth, 1 | 2 | 4 | 8 | 16),
                2 | 4 | 6 => matches!(bit_depth, 8 | 16),
                3 => matches!(bit_depth, 1 | 2 | 4 | 8),
                _ => false,
            };
            let bpp = if bit_depth == 16 { 8 } else { 4 };
            if !valid_format || !screenshot_dimensions_ok(width, height, bpp) {
                return false;
            }
            saw_ihdr = true;
        } else if ctype == b"IDAT" {
            if len > 0 {
                saw_idat = true;
            }
        } else if ctype == b"IEND" {
            if len != 0 || chunk_end != bytes.len() {
                return false;
            }
            saw_iend = true;
            break;
        }
        pos = chunk_end;
    }
    saw_ihdr && saw_idat && saw_iend
}

fn validate_jpeg(bytes: &[u8]) -> bool {
    use zune_jpeg::zune_core::{bytestream::ZCursor, options::DecoderOptions};
    use zune_jpeg::JpegDecoder;

    if !bytes.starts_with(b"\xff\xd8") || !bytes.ends_with(b"\xff\xd9") {
        return false;
    }
    let options = DecoderOptions::default()
        .set_max_width(MAX_SCREENSHOT_DIMENSION as usize)
        .set_max_height(MAX_SCREENSHOT_DIMENSION as usize)
        .set_strict_mode(true);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    if decoder.decode_headers().is_err() {
        return false;
    }
    let Some(info) = decoder.info() else {
        return false;
    };
    // Enforce the decoded image budget before allocating pixels or decoding scans.
    screenshot_dimensions_ok(u32::from(info.width), u32::from(info.height), 4)
        && decoder.decode().is_ok()
}

fn validate_webp(bytes: &[u8]) -> bool {
    if bytes.len() < 12 || !bytes.starts_with(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return false;
    }
    let riff_size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    if riff_size.checked_add(8) != Some(bytes.len()) {
        return false;
    }
    let mut chunks = &bytes[12..];
    let mut canvas = None;
    let mut animated = false;
    let mut saw_animation_header = false;
    let mut saw_image = false;
    while !chunks.is_empty() {
        let first = chunks.len() == bytes.len() - 12;
        let Some((tag, data)) = webp_chunk(&mut chunks) else {
            return false;
        };
        match tag {
            b"VP8X" if first && data.len() == 10 => {
                let width = webp_u24(&data[4..7]) + 1;
                let height = webp_u24(&data[7..10]) + 1;
                if !screenshot_dimensions_ok(width, height, 4) {
                    return false;
                }
                canvas = Some((width, height));
                animated = data[0] & 0x02 != 0;
            }
            b"VP8 " | b"VP8L" if !animated && !saw_image => {
                let Some(dimensions) = webp_image_dimensions(tag, data) else {
                    return false;
                };
                if canvas.is_some_and(|canvas| canvas != dimensions) {
                    return false;
                }
                saw_image = true;
            }
            b"ANIM" if animated && !saw_animation_header && data.len() == 6 => {
                saw_animation_header = true;
            }
            b"ANMF"
                if saw_animation_header && canvas.is_some_and(|c| validate_webp_frame(data, c)) =>
            {
                saw_image = true;
            }
            b"VP8X" | b"VP8 " | b"VP8L" | b"ANIM" | b"ANMF" => return false,
            _ => {}
        }
    }
    saw_image
}

/// Read one length-delimited RIFF chunk, including its alignment padding.
fn webp_chunk<'a>(chunks: &mut &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
    let (header, rest) = chunks.split_at_checked(8)?;
    let len = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
    let (data, rest) = rest.split_at_checked(len)?;
    *chunks = rest.get(len & 1..)?;
    Some((&header[..4], data))
}

fn webp_u24(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0])
}

fn webp_image_dimensions(tag: &[u8], data: &[u8]) -> Option<(u32, u32)> {
    // Both bitstreams must contain bytes beyond their dimension headers.
    let (width, height) = match tag {
        b"VP8 " if data.len() > 10 && data[0] & 1 == 0 && data[3..6] == [0x9d, 0x01, 0x2a] => {
            let width = u16::from_le_bytes([data[6], data[7]]) & 0x3fff;
            let height = u16::from_le_bytes([data[8], data[9]]) & 0x3fff;
            (u32::from(width), u32::from(height))
        }
        b"VP8L" if data.len() > 5 && data[0] == 0x2f && data[4] >> 5 == 0 => {
            let bits = u32::from_le_bytes(data[1..5].try_into().ok()?);
            ((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1)
        }
        _ => return None,
    };
    screenshot_dimensions_ok(width, height, 4).then_some((width, height))
}

fn validate_webp_frame(data: &[u8], (canvas_width, canvas_height): (u32, u32)) -> bool {
    // The stored frame offsets are half their pixel values. RIFF subchunks follow the header.
    let Some((header, mut chunks)) = data.split_at_checked(16) else {
        return false;
    };
    let width = webp_u24(&header[6..9]) + 1;
    let height = webp_u24(&header[9..12]) + 1;
    if webp_u24(&header[..3]) * 2 + width > canvas_width
        || webp_u24(&header[3..6]) * 2 + height > canvas_height
    {
        return false;
    }
    let mut saw_image = false;
    while !chunks.is_empty() {
        let Some((tag, data)) = webp_chunk(&mut chunks) else {
            return false;
        };
        if matches!(tag, b"VP8 " | b"VP8L") {
            if saw_image || webp_image_dimensions(tag, data) != Some((width, height)) {
                return false;
            }
            saw_image = true;
        }
    }
    saw_image
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
    send_reply(&s, &id, &device, &session, &req.text, None).await
}

async fn send_screenshot(State(s): AppState, Path(id): Path<String>, request: Request) -> Response {
    let (device, file) = match authed(&s, request.headers()) {
        Ok(v) => v,
        Err(r) => return r,
    };
    if device.read_only {
        return error(StatusCode::FORBIDDEN, "read_only");
    }
    let Some((session, kind)) = s.screenshot_target(&file, &id) else {
        return error(StatusCode::NOT_FOUND, "not_found");
    };
    let Ok(permit) = s.screenshot_slots.clone().try_acquire_owned() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "screenshot_busy");
    };
    let (text, bytes) = {
        let body = match tokio::time::timeout(
            s.screenshot_read_timeout,
            axum::body::to_bytes(request.into_body(), MAX_SCREENSHOT_BODY),
        )
        .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(_)) => return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large"),
            Err(_) => return error(StatusCode::REQUEST_TIMEOUT, "screenshot_timeout"),
        };
        let Ok(req) = serde_json::from_slice::<ScreenshotReq<'_>>(&body) else {
            return error(StatusCode::BAD_REQUEST, "bad_request");
        };
        if req.image.len() > MAX_ENCODED_SCREENSHOT || req.text.len() > MAX_SEND_TEXT {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
        }
        let Ok(bytes) = STANDARD.decode(req.image) else {
            return error(StatusCode::BAD_REQUEST, "bad_request");
        };
        (req.text, bytes)
    };
    if bytes.len() > MAX_SCREENSHOT_BYTES {
        return error(StatusCode::PAYLOAD_TOO_LARGE, "too_large");
    }
    // Keep the slot with the decoder even if the client disconnects.
    let (_permit, bytes, extension) = match tokio::task::spawn_blocking(move || {
        let extension = screenshot_extension(&bytes);
        (permit, bytes, extension)
    })
    .await
    {
        Ok((permit, bytes, Some(extension))) => (permit, bytes, extension),
        Ok((_, _, None)) => return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_image"),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    };
    let screenshot = Screenshot {
        bytes,
        extension,
        kind,
    };
    send_reply(&s, &id, &device, &session, &text, Some(screenshot)).await
}

async fn send_reply(
    s: &Arc<Shared>,
    id: &str,
    device: &Device,
    session: &str,
    text: &str,
    screenshot: Option<Screenshot>,
) -> Response {
    match crate::tmux_bridge::try_live_sessions(&s.tmux).await {
        Ok(live) if live.contains(session) => {}
        Ok(_) => return error(StatusCode::CONFLICT, "stopped"),
        Err(e) if e.kind() == std::io::ErrorKind::NotConnected => {
            return error(StatusCode::CONFLICT, "stopped");
        }
        Err(e) => {
            eprintln!("oo-remote: list sessions failed: {e}");
            return error(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
        }
    }
    let hub = match tokio::time::timeout(HUB_OPEN_TIMEOUT, s.hubs.get(session)).await {
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
    let delay = s
        .test_send_delay_ms
        .load(std::sync::atomic::Ordering::SeqCst);
    if delay > 0 {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }
    // Opening the hub can take seconds: the mark must still map to this
    // session (and the device still be allowed) right before typing.
    if !still_allowed(s, &device.id, id, session) {
        return error(StatusCode::NOT_FOUND, "not_found");
    }
    let text = flatten_newlines(text);
    let result = if let Some(screenshot) = screenshot {
        send_screenshot_reply(s, &hub, device, id, session, &text, screenshot).await
    } else {
        hub.send_line(text.as_bytes()).await.map_err(|err| {
            eprintln!("oo-remote: send failed: {err}");
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        })
    };
    match result {
        Ok(()) => {
            s.input.record(session, &device.name, SystemTime::now());
            StatusCode::NO_CONTENT.into_response()
        }
        Err((status, code)) => error(status, code),
    }
}

async fn save_temp_screenshot(
    state_dir: std::path::PathBuf,
    extension: &'static str,
    bytes: Vec<u8>,
) -> Result<crate::store::PendingScreenshot, (StatusCode, &'static str)> {
    match tokio::task::spawn_blocking(move || {
        crate::store::save_screenshot(&state_dir, extension, &bytes)
    })
    .await
    {
        Ok(Ok(saved)) => Ok(saved),
        Ok(Err(e)) if e.kind() == io::ErrorKind::StorageFull => {
            Err((StatusCode::INSUFFICIENT_STORAGE, "screenshot_storage_full"))
        }
        _ => Err((StatusCode::SERVICE_UNAVAILABLE, "unavailable")),
    }
}

async fn send_screenshot_reply(
    s: &Arc<Shared>,
    hub: &crate::tmux_bridge::SessionHub,
    device: &Device,
    id: &str,
    session: &str,
    text: &str,
    screenshot: Screenshot,
) -> Result<(), (StatusCode, &'static str)> {
    use crate::tmux_bridge::SendLineError;

    let expected_kind = screenshot.kind;
    let state_dir = s.state_dir.clone();
    // The path is sent as terminal input, so reject terminal control characters.
    if !state_dir.is_absolute()
        || state_dir
            .to_str()
            .is_none_or(|path| path.chars().any(char::is_control))
    {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "unavailable"));
    }
    let saved = save_temp_screenshot(state_dir, screenshot.extension, screenshot.bytes).await?;
    let Some(native_image) = screenshot_still_allowed(s, &device.id, id, session, &expected_kind)
    else {
        return Err((StatusCode::NOT_FOUND, "not_found"));
    };
    let text = if text.trim().is_empty() {
        "Please inspect the attached screenshot."
    } else {
        text
    };
    let auth_shared = s.clone();
    let auth_device = device.id.clone();
    let auth_id = id.to_string();
    let auth_session = session.to_string();
    let auth_kind = expected_kind;
    let authorize = move || {
        screenshot_still_allowed(
            &auth_shared,
            &auth_device,
            &auth_id,
            &auth_session,
            &auth_kind,
        )
        .is_some()
    };
    match hub
        .send_screenshot(text, saved, native_image, authorize)
        .await
    {
        Ok(()) => Ok(()),
        Err(SendLineError::Unauthorized) => Err((StatusCode::NOT_FOUND, "not_found")),
        Err(SendLineError::NotWritten(_) | SendLineError::PartialOrUnknown(_)) => {
            Err((StatusCode::SERVICE_UNAVAILABLE, "unavailable"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use futures_util::StreamExt;
    use std::path::Path as FsPath;
    use tower::ServiceExt;

    const TEST_PNG: &str =
        "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgZGIGAAAOAAfXb+R4AAAAAElFTkSuQmCC";

    fn paired_fixture(dir: &FsPath, tmux: crate::Tmux) -> (Shared, String, String, String) {
        use crate::{certs, snapshot_channel, AgentInfo, ServerConfig, Snapshot, Store};

        let store = Store::new(dir);
        let now = SystemTime::now();
        let pair = || {
            let code = auth::issue_code(&store, now).unwrap();
            auth::redeem_code(&store, &code, "test phone", "localhost", now).unwrap()
        };
        let (_, writer) = pair();
        let (dev, reader) = pair();
        auth::set_read_only(&store, &dev.id, true).unwrap();
        let id = store.mark("test", 1).unwrap();
        let shell_id = store.mark("test", 2).unwrap();
        let agent = AgentInfo {
            id: id.clone(),
            title: "Codex".into(),
            agent_kind: Some("codex".into()),
            project: "test".into(),
            worktree: "main".into(),
            status: "idle".into(),
            status_since: None,
            tool: None,
            branch: None,
            has_conversation: false,
            session: "s_codex".into(),
            transcript: None,
        };
        let (snap, rx) = snapshot_channel();
        snap.send_replace(Snapshot {
            agents: vec![
                AgentInfo {
                    id: shell_id,
                    agent_kind: None,
                    ..agent.clone()
                },
                agent,
            ],
        });
        let cfg = ServerConfig {
            state_dir: dir.to_path_buf(),
            port: 0,
            setup_port: 0,
            tmux,
            bind_ip: "127.0.0.1".parse().unwrap(),
            hosts_override: None,
        };
        let ca = certs::load_or_create_ca(&dir.join("remote"), "Test Mac", &[], &[]).unwrap();
        let s = Shared::new(&cfg, rx, 0, &ca, "Test Mac".into(), Duration::from_secs(60));
        (s, id, writer, reader)
    }

    async fn upload(app: &Router, id: &str, token: Option<&str>, image: &str) -> StatusCode {
        let mut builder = Request::builder()
            .method("POST")
            .uri(format!("/api/agents/{id}/send-screenshot"))
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = builder
            .body(Body::from(json!({"image": image, "text": ""}).to_string()))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        app.clone().oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn screenshot_requires_write_access_and_an_image() {
        let dir = tempfile::tempdir().unwrap();
        let no_tmux = crate::Tmux {
            socket: Some(crate::store::new_id("oo-unstarted-test-").unwrap()),
        };
        let (shared, id, writer, reader) = paired_fixture(dir.path(), no_tmux);
        let app = router(Arc::new(shared));
        assert_eq!(upload(&app, &id, None, TEST_PNG).await, 401);
        assert_eq!(upload(&app, &id, Some(&reader), TEST_PNG).await, 403);
        let store = crate::Store::new(dir.path());
        let shell = store.mark("test", 2).unwrap();
        assert_eq!(upload(&app, &shell, Some(&writer), TEST_PNG).await, 404);
        assert_eq!(upload(&app, &id, Some(&writer), "aGVsbG8=").await, 415);
        let large_image = "A".repeat(MAX_SCREENSHOT_BODY);
        assert_eq!(upload(&app, &id, Some(&writer), &large_image).await, 413);
        store.unmark("test", Some(1)).unwrap();
        assert_eq!(upload(&app, &id, Some(&writer), TEST_PNG).await, 404);
        assert!(!dir.path().join("remote/screenshots").exists());
    }

    #[tokio::test]
    async fn screenshot_rejects_truncated_and_oversized_images() {
        let dir = tempfile::tempdir().unwrap();
        let no_tmux = crate::Tmux {
            socket: Some(crate::store::new_id("oo-unstarted-test-").unwrap()),
        };
        let (shared, id, writer, _) = paired_fixture(dir.path(), no_tmux);
        let app = router(Arc::new(shared));

        // Signature-only PNG and truncated PNG must be rejected with 415 before saving.
        let sig_only = STANDARD.encode(b"\x89PNG\r\n\x1a\n");
        assert_eq!(upload(&app, &id, Some(&writer), &sig_only).await, 415);
        let mut raw_png = STANDARD.decode(TEST_PNG).unwrap();
        raw_png.truncate(raw_png.len() - 4);
        let truncated_png = STANDARD.encode(&raw_png);
        assert_eq!(upload(&app, &id, Some(&writer), &truncated_png).await, 415);

        // Oversized dimensions in IHDR (valid CRC) must be rejected with 415.
        let mut huge_png = STANDARD.decode(TEST_PNG).unwrap();
        huge_png[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        huge_png[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        let ihdr_crc = crc32_ieee(&huge_png[12..29]);
        huge_png[29..33].copy_from_slice(&ihdr_crc.to_be_bytes());
        assert_eq!(
            upload(&app, &id, Some(&writer), &STANDARD.encode(&huge_png)).await,
            415
        );
        assert!(!dir.path().join("remote/screenshots").exists());
    }

    #[test]
    fn screenshot_validates_jpeg_and_webp_containers() {
        for jpeg in [
            include_bytes!("../tests/fixtures/screenshot-baseline.jpg").as_slice(),
            include_bytes!("../tests/fixtures/screenshot-progressive.jpg").as_slice(),
        ] {
            assert_eq!(screenshot_extension(jpeg), Some("jpg"));
            assert_eq!(screenshot_extension(&jpeg[..jpeg.len() - 2]), None);
            let scan = jpeg
                .windows(2)
                .position(|marker| marker == b"\xff\xda")
                .unwrap();
            let mut no_scan = jpeg[..scan].to_vec();
            no_scan.extend_from_slice(b"\xff\xd9");
            assert_eq!(screenshot_extension(&no_scan), None);
        }

        // One-pixel WebP fixtures from cwebp and webpmux.
        for encoded in [
            "UklGRiQAAABXRUJQVlA4IBgAAAAwAQCdASoBAAEAAgA0JaQAA3AA/vvfkAA=",
            "UklGRiAAAABXRUJQVlA4TBQAAAAvAAAAAAdQgVQIIAAKmv7HiIj+Bw==",
            "UklGRkgAAABXRUJQVlA4WAoAAAAIAAAAAAAAAAAAVlA4TBQAAAAvAAAAAAdQgVQIIAAKmv7HiIj+B0VYSUYOAAAAUDYKMSAxCjI1NQoBAgM=",
            concat!(
                "UklGRo4AAABXRUJQVlA4WAoAAAACAAAAAAAAAAAAQU5JTQYAAAD/////AABBTk1GLAAAAAAAAAAAAAAA",
                "AAAAAGQAAABWUDhMFAAAAC8AAAAAB1CBVAggAAqa/seIiP4HQU5NRi4AAAAAAAAAAAAAAAAAAABkAAAA",
                "VlA4TBUAAAAvAAAAAAfQAgJJW7ztr0H/44iI/gcA",
            ),
        ] {
            let webp = STANDARD.decode(encoded).unwrap();
            assert_eq!(screenshot_extension(&webp), Some("webp"));
            assert_eq!(screenshot_extension(&webp[..webp.len() - 2]), None);
        }
    }

    #[test]
    fn screenshot_rejects_webp_containers_without_image_data() {
        let extended_header = b"RIFF\x16\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0\0\0\0\0\0\0";
        let lossless_header = b"RIFF\x12\0\0\0WEBPVP8L\x05\0\0\0\x2f\0\0\0\0\0";
        let mut empty_frame = extended_header.to_vec();
        empty_frame[20] = 0x02; // Animation flag.
        empty_frame.extend_from_slice(b"ANIM\x06\0\0\0\0\0\0\0\0\0ANMF\x10\0\0\0");
        empty_frame.extend_from_slice(&[0; 16]);
        let riff_size = (empty_frame.len() - 8) as u32;
        empty_frame[4..8].copy_from_slice(&riff_size.to_le_bytes());

        for webp in [
            extended_header.as_slice(),
            lossless_header.as_slice(),
            &empty_frame,
        ] {
            assert_eq!(screenshot_extension(webp), None);
        }
    }

    #[test]
    fn screenshot_still_allowed_rejects_kind_change() {
        use crate::{snapshot_channel, Snapshot};

        let dir = tempfile::tempdir().unwrap();
        let (mut shared, id, _, _) = paired_fixture(dir.path(), crate::Tmux::default());
        let writer_id = shared.current_file().devices[0].id.clone();
        let mut agent = shared.snapshot.borrow().agents[1].clone();
        let (tx, rx) = snapshot_channel();
        tx.send(Snapshot {
            agents: vec![agent.clone()],
        })
        .unwrap();
        shared.snapshot = rx;

        assert_eq!(
            screenshot_still_allowed(&shared, &writer_id, &id, "s_codex", "codex"),
            Some(true)
        );
        agent.agent_kind = Some("claude".into());
        tx.send(Snapshot {
            agents: vec![agent],
        })
        .unwrap();
        assert_eq!(
            screenshot_still_allowed(&shared, &writer_id, &id, "s_codex", "codex"),
            None
        );
    }

    #[test]
    fn cancelled_save_cleans_up_screenshot_file() {
        use std::future::Future;
        use std::task::{Context, Waker};

        let dir = tempfile::tempdir().unwrap();
        let bytes = STANDARD.decode(TEST_PNG).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            // Occupy the only blocking worker so the save is queued when canceled.
            let (release, hold) = std::sync::mpsc::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = hold.recv();
            });
            let mut save = Box::pin(save_temp_screenshot(dir.path().to_path_buf(), "png", bytes));
            assert!(save
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending());
            drop(save);
            release.send(()).unwrap();
            blocker.await.unwrap();
            tokio::task::spawn_blocking(|| {}).await.unwrap();
        });
        let screenshots = dir.path().join("remote/screenshots");
        assert!(screenshots.join(".lock").exists(), "queued save ran");
        assert_eq!(std::fs::read_dir(screenshots).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn screenshot_limits_concurrency_and_times_out_stalled_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let no_tmux = crate::Tmux {
            socket: Some(crate::store::new_id("oo-unstarted-test-").unwrap()),
        };
        let (mut shared, id, writer, _) = paired_fixture(dir.path(), no_tmux);
        shared.screenshot_read_timeout = Duration::from_millis(20);
        let shared = Arc::new(shared);
        let app = router(shared.clone());

        let mut stalled = Request::builder()
            .method("POST")
            .uri(format!("/api/agents/{id}/send-screenshot"))
            .header(header::AUTHORIZATION, format!("Bearer {writer}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from_stream(stream::pending::<
                Result<Bytes, Infallible>,
            >()))
            .unwrap();
        stalled
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        assert_eq!(app.clone().oneshot(stalled).await.unwrap().status(), 408);

        let _permits = shared
            .screenshot_slots
            .acquire_many(MAX_CONCURRENT_SCREENSHOTS as u32)
            .await
            .unwrap();
        assert_eq!(upload(&app, &id, Some(&writer), TEST_PNG).await, 429);
    }

    #[tokio::test]
    #[ignore = "requires tmux"]
    async fn screenshot_upload_delivers_image_path_and_caption_to_codex() {
        use std::os::unix::fs::PermissionsExt;

        struct PrivateTmux(crate::Tmux);
        impl Drop for PrivateTmux {
            fn drop(&mut self) {
                let _ = std::process::Command::new("tmux")
                    .args(["-L", self.0.socket.as_deref().unwrap(), "kill-server"])
                    .output();
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let tmux = PrivateTmux(crate::Tmux {
            socket: Some(crate::store::new_id("oo-screenshot-test-").unwrap()),
        });
        let capture = dir.path().join("input");
        let quoted = capture.to_str().unwrap().replace('\'', "'\\''");
        let status = tmux
            .0
            .command()
            .args(["-f", "/dev/null", "new-session", "-d", "-s", "s_codex"])
            .arg(format!("stty raw -echo && exec cat > '{quoted}'"))
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let deadline = Instant::now() + Duration::from_secs(3);
        while !capture.exists() {
            assert!(Instant::now() < deadline, "pane didn't start");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let (shared, id, writer, _) = paired_fixture(dir.path(), tmux.0.clone());
        let app = router(Arc::new(shared));
        assert_eq!(upload(&app, &id, Some(&writer), TEST_PNG).await, 204);
        let image = std::fs::read_dir(dir.path().join("remote/screenshots"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "png"))
            .expect("screenshot saved as PNG");
        assert_eq!(
            std::fs::read(&image).unwrap(),
            STANDARD.decode(TEST_PNG).unwrap()
        );
        assert_eq!(
            std::fs::metadata(&image).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let expected = format!(
            "\x1b[200~{}\x1b[201~ Please inspect the attached screenshot.\r",
            image.display()
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = std::fs::read(&capture).unwrap();
            if received.ends_with(b"\r") {
                assert_eq!(received, expected.as_bytes());
                break;
            }
            assert!(
                Instant::now() < deadline,
                "pane didn't receive the full reply: {received:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn voice_fixture(probe_status: u8) -> (tempfile::TempDir, Router, String, String) {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let (mut shared, _, writer, reader) = paired_fixture(dir.path(), crate::Tmux::default());
        let helper = dir.path().join("voice-test");
        std::fs::write(
            &helper,
            format!("#!/bin/sh\ncase \"$1\" in --available) exit {probe_status};; esac\ntest -r \"$1\" || exit 3\nprintf 'test dictated draft\\n'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        shared.voice.use_test_helper(helper);
        (dir, router(Arc::new(shared)), writer, reader)
    }

    async fn device_capabilities(app: &Router, token: &str) -> serde_json::Value {
        let mut request = Request::builder()
            .uri("/api/me")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn voice_is_not_advertised_for_an_unsupported_locale() {
        let (_dir, app, writer, _) = voice_fixture(4);
        assert!(device_capabilities(&app, &writer)
            .await
            .get("voice_input")
            .is_none());

        let mut request = Request::builder()
            .method("POST")
            .uri("/api/voice")
            .header(header::AUTHORIZATION, format!("Bearer {writer}"))
            .header(header::CONTENT_TYPE, "audio/wav")
            .body(Body::empty())
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
        assert_eq!(app.oneshot(request).await.unwrap().status(), 503);
    }

    #[tokio::test]
    async fn voice_requires_write_permission_and_returns_an_uncached_draft() {
        const WAV: &[u8] = b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x02\0\0\0\0\0";
        let (_dir, app, writer, reader) = voice_fixture(0);
        assert_eq!(
            device_capabilities(&app, &writer).await["voice_input"],
            true
        );
        assert!(device_capabilities(&app, &reader)
            .await
            .get("voice_input")
            .is_none());
        async fn post(app: &Router, token: Option<&str>, audio: &'static [u8]) -> Response {
            let mut builder = Request::builder()
                .method("POST")
                .uri("/api/voice")
                .header(header::CONTENT_TYPE, "audio/wav");
            if let Some(token) = token {
                builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            let mut request = builder.body(Body::from(audio)).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));
            app.clone().oneshot(request).await.unwrap()
        }
        assert_eq!(post(&app, None, WAV).await.status(), 401);
        assert_eq!(post(&app, Some(&reader), WAV).await.status(), 403);
        assert_eq!(post(&app, Some(&writer), b"bad audio").await.status(), 422);
        let response = post(&app, Some(&writer), WAV).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 100)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&body).unwrap(),
            json!({"text": "test dictated draft"})
        );
    }

    #[tokio::test]
    async fn voice_rechecks_write_access_before_starting_helper() {
        use std::os::unix::fs::PermissionsExt;

        const WAV: &[u8] = b"RIFF\x26\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0\x80\x3e\0\0\0\x7d\0\0\x02\0\x10\0data\x02\0\0\0\0\0";
        let dir = tempfile::tempdir().unwrap();
        let (mut shared, _, writer, _) = paired_fixture(dir.path(), crate::Tmux::default());
        let marker = dir.path().join("helper-ran");
        let helper = dir.path().join("voice-test");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\ncase \"$1\" in --available) exit 0;; esac\n: > '{}'\nprintf 'ok\\n'\n",
                marker.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
        shared.voice.use_test_helper(helper);
        let shared = Arc::new(shared);
        let app = router(shared.clone());

        let (started, reading) = tokio::sync::oneshot::channel();
        let (rest, rest_rx) = tokio::sync::oneshot::channel();
        let body_stream = stream::once(async move {
            started.send(()).unwrap();
            Ok::<_, Infallible>(Bytes::from_static(&WAV[..20]))
        })
        .chain(stream::once(async move {
            rest_rx.await.unwrap();
            Ok(Bytes::from_static(&WAV[20..]))
        }));
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/voice")
            .header(header::AUTHORIZATION, format!("Bearer {writer}"))
            .header(header::CONTENT_TYPE, "audio/wav")
            .body(Body::from_stream(body_stream))
            .unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:1234".parse::<SocketAddr>().unwrap()));

        let resp_task = tokio::spawn(async move { app.oneshot(request).await.unwrap() });
        tokio::time::timeout(Duration::from_secs(2), reading)
            .await
            .unwrap()
            .unwrap();
        let writer_id = shared.current_file().devices[0].id.clone();
        auth::revoke(&shared.store(), &writer_id).unwrap();
        rest.send(()).unwrap();

        let resp = resp_task.await.unwrap();
        assert_eq!(resp.status(), 401);
        assert!(
            !marker.exists(),
            "helper must not run after device revocation"
        );
    }

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
    async fn every_response_carries_security_headers() {
        // Outside the peer filter, like the real routers: a refused peer's
        // 403 gets them too.
        let app = Router::new()
            .fallback(|| async { "ok" })
            .layer(middleware::from_fn(peer_filter))
            .layer(middleware::from_fn(security_headers));
        for ip in ["8.8.8.8:1234", "192.168.1.9:1234"] {
            let mut req = Request::builder().uri("/x").body(Body::empty()).unwrap();
            req.extensions_mut()
                .insert(ConnectInfo(ip.parse::<SocketAddr>().unwrap()));
            let resp = app.clone().oneshot(req).await.unwrap();
            let h = resp.headers();
            assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{ip}");
            assert_eq!(h[header::REFERRER_POLICY], "no-referrer", "{ip}");
            assert_eq!(h[header::X_FRAME_OPTIONS], "DENY", "{ip}");
            assert!(h.get(header::STRICT_TRANSPORT_SECURITY).is_none());
            assert_eq!(h.get_all(header::X_CONTENT_TYPE_OPTIONS).iter().count(), 1);
        }
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
