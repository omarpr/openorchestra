//! Conversation view routes (bearer auth; read-only devices may read).
//!
//! - `GET /api/agents/{id}/conversation` (SSE): `event: entries` with
//!   `{"replace":true,"entries":[…]}` on connect, then
//!   `{"replace":false,"entries":[…]}` (merge by id: known ids replace in
//!   place, unknown ids append; keep the last 200) or `{"replace":true,…}`
//!   (replace the whole list) as the transcript changes. A new transcript
//!   for the id sends a fresh `replace:true`. Ends with `event: closed`
//!   `{"reason":"unmarked"|"revoked"|"disabled"}`. `:keepalive` every 15 s.
//!   404 for an unknown/unmarked id or one without a supported transcript.
//! - `GET /api/agents/{id}/conversation/tool/{tool_id}`:
//!   `{"text","is_error","truncated"}` (text capped at 64 KiB); 404 when
//!   unknown.
//!
//! The transcript path comes only from the app's snapshot for a currently
//! marked id (`Shared::transcript_for`), is re-checked on every request and
//! on every remote.json / snapshot change, and is never sent or logged.

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures_util::stream::{self, Stream};
use serde_json::json;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio::sync::watch;

use crate::api::{authed, closed_event, error, AppState, SSE_KEEPALIVE};
use crate::conversation::{Entry, TranscriptKind};
use crate::server::{Shared, Snapshot};
use crate::transcript::{Tail, Update};
use crate::RemoteFile;

/// Tool ids are short (`toolu_…`, pi call ids); anything longer is unknown.
const MAX_TOOL_ID: usize = 256;

type Target = (PathBuf, TranscriptKind);

fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not_found")
}

fn entries_event(replace: bool, entries: &[Entry]) -> Event {
    Event::default()
        .event("entries")
        .data(json!({ "replace": replace, "entries": entries }).to_string())
}

/// The transcript for `id` per a fresh read of remote.json, re-checked after
/// the (possibly slow) first read of the tail.
async fn open_tail(s: &Arc<Shared>, file: &RemoteFile, id: &str) -> Option<(Target, Arc<Tail>)> {
    let target = s.transcript_for(file, id)?;
    let tail = s.transcripts.get(target.0.clone(), target.1).await;
    (s.transcript_for(&s.current_file(), id).as_ref() == Some(&target)).then_some((target, tail))
}

pub(crate) async fn stream(
    State(s): AppState,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    // Subscribe before authenticating so a revoke/unmark racing the request
    // is still seen as a change.
    let stop = s.shutdown.subscribe();
    let file_rx = s.file.subscribe();
    let mut snap = s.snapshot.clone();
    snap.borrow_and_update();
    let (device, file) = match authed(&s, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some((target, tail)) = open_tail(&s, &file, &id).await else {
        return not_found();
    };
    let (window, updates) = tail.subscribe();
    let st = St {
        _open: s.open_streams.clone(),
        s,
        id,
        device_id: device.id,
        stop,
        file: file_rx,
        snap,
        snap_open: true,
        target,
        tail,
        updates,
        first: Some(window),
        done: false,
    };
    Sse::new(events(st))
        .keep_alive(KeepAlive::new().interval(SSE_KEEPALIVE).text("keepalive"))
        .into_response()
}

struct St {
    s: Arc<Shared>,
    id: String,
    device_id: String,
    stop: watch::Receiver<bool>,
    file: watch::Receiver<Arc<RemoteFile>>,
    snap: watch::Receiver<Snapshot>,
    /// False once the app dropped its snapshot sender.
    snap_open: bool,
    target: Target,
    /// Held so the tailer keeps running while this stream is open.
    tail: Arc<Tail>,
    updates: broadcast::Receiver<Update>,
    first: Option<Vec<Entry>>,
    done: bool,
    _open: Arc<()>,
}

impl St {
    /// Start over from the tail's current window.
    fn resubscribe(&mut self) -> Event {
        let (window, updates) = self.tail.subscribe();
        self.updates = updates;
        entries_event(true, &window)
    }

    fn close(&mut self, reason: &str) -> Event {
        self.done = true;
        closed_event(reason)
    }
}

fn events(st: St) -> impl Stream<Item = Result<Event, Infallible>> {
    stream::unfold(st, |mut st| async move {
        if st.done {
            return None;
        }
        if let Some(window) = st.first.take() {
            return Some((Ok(entries_event(true, &window)), st));
        }
        loop {
            tokio::select! {
                biased;
                _ = crate::server::stopped(&mut st.stop) => {
                    let ev = st.close("disabled");
                    return Some((Ok(ev), st));
                }
                r = st.file.changed() => {
                    if r.is_err() {
                        return None;
                    }
                    let (revoked, unmarked) = {
                        let f = st.file.borrow_and_update();
                        (
                            !f.devices.iter().any(|d| d.id == st.device_id),
                            f.mark_by_id(&st.id).is_none(),
                        )
                    };
                    if revoked {
                        let ev = st.close("revoked");
                        return Some((Ok(ev), st));
                    }
                    if unmarked {
                        let ev = st.close("unmarked");
                        return Some((Ok(ev), st));
                    }
                }
                r = st.snap.changed(), if st.snap_open => {
                    if r.is_err() {
                        // The app dropped its sender: keep the last snapshot.
                        st.snap_open = false;
                        continue;
                    }
                    st.snap.borrow_and_update();
                    let file = st.s.current_file();
                    if !file.devices.iter().any(|d| d.id == st.device_id) {
                        let ev = st.close("revoked");
                        return Some((Ok(ev), st));
                    }
                    let Some(target) = st.s.transcript_for(&file, &st.id) else {
                        // The app no longer vouches for a transcript here.
                        let ev = st.close("unmarked");
                        return Some((Ok(ev), st));
                    };
                    if target == st.target {
                        // Status-only change.
                        continue;
                    }
                    st.tail = st.s.transcripts.get(target.0.clone(), target.1).await;
                    st.target = target;
                    let ev = st.resubscribe();
                    return Some((Ok(ev), st));
                }
                u = st.updates.recv() => match u {
                    Ok(Update::Upsert(es)) => {
                        return Some((Ok(entries_event(false, &es)), st));
                    }
                    Ok(Update::Replace(es)) => {
                        return Some((Ok(entries_event(true, &es)), st));
                    }
                    Err(RecvError::Lagged(_)) => {
                        let ev = st.resubscribe();
                        return Some((Ok(ev), st));
                    }
                    Err(RecvError::Closed) => return None,
                },
            }
        }
    })
}

/// One tool call's full result. 404 for an unknown/unmarked id, an id
/// without a supported transcript, or an unknown tool id.
pub(crate) async fn tool(
    State(s): AppState,
    Path((id, tool_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let file = match authed(&s, &headers) {
        Ok((_, file)) => file,
        Err(r) => return r,
    };
    if tool_id.len() > MAX_TOOL_ID {
        return not_found();
    }
    let Some((_, tail)) = open_tail(&s, &file, &id).await else {
        return not_found();
    };
    match tail.tool_result(&tool_id).await {
        Ok(Some(r)) => Json(r).into_response(),
        Ok(None) => not_found(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => not_found(),
        // No detail: an io error may name the file.
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    }
}
