//! Conversation view routes (bearer auth; read-only devices may read).
//!
//! - `GET /api/agents/{id}/conversation` (SSE): `event: entries` with
//!   `{"replace":true,"entries":[…]}` on connect, then
//!   `{"replace":false,"entries":[…]}` (merge by id: known ids replace in
//!   place, unknown ids append; keep the last 200) or `{"replace":true,…}`
//!   (replace the whole list) as the transcript changes. A new transcript
//!   for the id sends a fresh `replace:true`; if the app stops giving the
//!   (still marked, still listed) id a supported transcript, the stream
//!   sends `replace:true` with `[]` and waits for one. Ends with
//!   `event: closed` `{"reason":"unmarked"|"revoked"|"disabled"}` (unmarked:
//!   the mark is gone or the app no longer lists the id). `:keepalive`
//!   every 15 s.
//!   404 for an unknown/unmarked id or one without a supported transcript.
//! - `GET /api/agents/{id}/conversation/tool/{tool_id}`:
//!   `{"text","is_error","truncated"}` with optional executed `input` (each
//!   capped at 64 KiB); 404 when
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
use crate::conversation::{self, Entry, TranscriptKind};
use crate::server::{Shared, Snapshot};
use crate::transcript::{read_capped, Tail, Update, READ_CAP};
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
        target: Some(target),
        tail: Some(tail),
        updates: Some(updates),
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
    /// `None` while the id is marked and listed but the app gives it no
    /// supported transcript (the stream waits for one).
    target: Option<Target>,
    /// Held so the tailer keeps running while this stream is open.
    tail: Option<Arc<Tail>>,
    updates: Option<broadcast::Receiver<Update>>,
    first: Option<Vec<Entry>>,
    done: bool,
    _open: Arc<()>,
}

impl St {
    /// Start over from the tail's current window (empty without a tail).
    fn resubscribe(&mut self) -> Event {
        match &self.tail {
            Some(tail) => {
                let (window, updates) = tail.subscribe();
                self.updates = Some(updates);
                entries_event(true, &window)
            }
            None => {
                self.updates = None;
                entries_event(true, &[])
            }
        }
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
                    // Like the terminal: gone from the marks or the app's
                    // snapshot means unmarked.
                    let listed = st.s.snapshot.borrow().agents.iter().any(|a| a.id == st.id);
                    if file.mark_by_id(&st.id).is_none() || !listed {
                        let ev = st.close("unmarked");
                        return Some((Ok(ev), st));
                    }
                    let Some(target) = st.s.transcript_for(&file, &st.id) else {
                        // Listed without a supported transcript: stop
                        // reading the old file, send an empty list and wait.
                        if st.target.is_none() {
                            continue;
                        }
                        st.target = None;
                        st.tail = None;
                        let ev = st.resubscribe();
                        return Some((Ok(ev), st));
                    };
                    if st.target.as_ref() == Some(&target) {
                        // Status-only change.
                        continue;
                    }
                    let tail = st.s.transcripts.get(target.0.clone(), target.1).await;
                    // The first read can be slow: re-check before sending.
                    // A change that broke it wakes this loop again.
                    if st.s.transcript_for(&st.s.current_file(), &st.id).as_ref() != Some(&target) {
                        continue;
                    }
                    st.tail = Some(tail);
                    st.target = Some(target);
                    let ev = st.resubscribe();
                    return Some((Ok(ev), st));
                }
                u = next_update(&mut st.updates) => match u {
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

/// The next tail update; never resolves without a tail.
async fn next_update(rx: &mut Option<broadcast::Receiver<Update>>) -> Result<Update, RecvError> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
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
    let Some((path, kind)) = s.transcript_for(&file, &id) else {
        return not_found();
    };
    // A direct capped read: no tailer is started for a one-off fetch.
    let result = tokio::task::spawn_blocking(move || {
        let text = read_capped(&path, READ_CAP)?;
        Ok::<_, std::io::Error>(conversation::tool_result(kind, &text, &tool_id))
    })
    .await;
    match result {
        Ok(Ok(Some(r))) => Json(r).into_response(),
        Ok(Ok(None)) => not_found(),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => not_found(),
        // No detail: an io error may name the file.
        _ => error(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    }
}
