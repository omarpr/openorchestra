//! Shared transcript tailer: one background task per transcript file, shared
//! by every viewer, that turns file changes into [`Update`]s.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, SystemTime};

use notify::{Event, RecursiveMode, Watcher};
use tokio::sync::{broadcast, Notify};
use tokio::task::JoinHandle;

use crate::conversation::{self, Entry, EntryKind, ToolResult, TranscriptKind};

/// Entries kept in the window sent to viewers (the last N of the branch).
pub const WINDOW: usize = 200;
/// At most this many trailing bytes of a transcript are ever read.
pub const READ_CAP: u64 = 8 * 1024 * 1024;
const POLL: Duration = Duration::from_secs(1);
/// After a file-change wake, wait this long so a burst of writes collapses
/// into one re-read.
const COALESCE: Duration = Duration::from_millis(150);
const CHANNEL_CAP: usize = 64;
const CWD_SCAN_LINES: usize = 20;
const UNREADABLE: &str = "Can't read the conversation right now";

/// Use the session name unless a transcript/flag plus its atomic-write suffix
/// would exceed the filesystem's 255-byte filename limit. Agent shims use
/// the same mapping for their files.
pub fn session_file_stem(session: &str) -> std::borrow::Cow<'_, str> {
    use sha2::{Digest, Sha256};

    const MAX_STEM_BYTES: usize = 230;
    if session.len() <= MAX_STEM_BYTES {
        session.into()
    } else {
        format!("long-{:x}", Sha256::digest(session.as_bytes())).into()
    }
}

/// Turn on local transcript writers for exactly the shared tmux tabs.
pub(crate) fn sync_log_flags(
    dir: &Path,
    shared: &HashSet<String>,
    remove_unshared: bool,
) -> io::Result<()> {
    const FLAG_SUFFIX: &str = ".jsonl.on";
    let shared: HashSet<_> = shared
        .iter()
        .map(|session| session_file_stem(session))
        .collect();
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if let Some(session) = name.strip_suffix(FLAG_SUFFIX) {
            if !shared.contains(session) {
                fs::remove_file(dir.join(&name))?;
            }
        }
    }
    if remove_unshared {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let session = name.strip_suffix(".jsonl").or_else(|| {
                let (session, tmp) = name.split_once(".jsonl.")?;
                tmp.ends_with(".tmp").then_some(session)
            });
            if session.is_some_and(|session| !shared.contains(session)) {
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(err),
                }
            }
        }
    }
    for session in shared {
        let flag = dir.join(format!("{session}{FLAG_SUFFIX}"));
        if !flag.exists() {
            crate::store::write_atomic_private(&flag, b"")?;
        }
    }
    Ok(())
}

/// A change to the window a viewer holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// The whole window (connect, reset, fork, new file).
    Replace(Vec<Entry>),
    /// Changed-by-id entries plus new entries, in order; the viewer replaces
    /// entries with a known id and appends the rest.
    Upsert(Vec<Entry>),
}

/// The last [`WINDOW`] entries of `entries`.
pub fn window(entries: &[Entry]) -> Vec<Entry> {
    entries[entries.len().saturating_sub(WINDOW)..].to_vec()
}

/// What a viewer holding `prev` needs to reach `next`. `None` if equal.
/// `Upsert` when the entries of `prev` that are still in the window are an
/// unchanged-order prefix of `next` (entries only slid off the front, were
/// updated in place, or were appended); otherwise `Replace`.
pub fn diff(prev: &[Entry], next: &[Entry]) -> Option<Update> {
    let next = window(next);
    if prev == next.as_slice() {
        return None;
    }
    if !prev.is_empty() {
        let kept: Vec<&Entry> = match next.first() {
            Some(first) => match prev.iter().position(|e| e.id == first.id) {
                Some(k) => prev[k..].iter().collect(),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        let prefix_ok = !kept.is_empty()
            && kept.len() <= next.len()
            && kept.iter().zip(&next).all(|(p, n)| p.id == n.id);
        if prefix_ok {
            // Front entries slid off a window that wasn't full: viewers
            // can't drop them via Upsert, so reset.
            if prev.len() < WINDOW && kept.len() < prev.len() {
                return Some(Update::Replace(next));
            }
            let changed: Vec<Entry> = next
                .iter()
                .enumerate()
                .filter(|(i, n)| kept.get(*i).is_none_or(|p| *p != *n))
                .map(|(_, n)| n.clone())
                .collect();
            if !changed.is_empty() {
                return Some(Update::Upsert(changed));
            }
        }
    }
    Some(Update::Replace(next))
}

/// The whole file if it is at most `cap` bytes, else its last `cap` bytes
/// after the first newline (so the partial first line is dropped).
pub fn read_capped(path: &Path, cap: u64) -> io::Result<String> {
    let mut f = fs::File::open(path)?;
    if path.extension().is_some_and(|ext| ext == "zst") {
        let mut reader = zstd::stream::read::Decoder::new(f)?;
        let mut tail = std::collections::VecDeque::new();
        let mut chunk = [0; 32768];
        let mut truncated = false;
        loop {
            let n = reader.read(&mut chunk)?;
            if n == 0 {
                break;
            }
            tail.extend(&chunk[..n]);
            let excess = tail.len().saturating_sub(cap as usize);
            truncated |= excess > 0;
            tail.drain(..excess);
        }
        let bytes = tail.make_contiguous();
        let start = if truncated {
            bytes
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| i + 1)
        } else {
            0
        };
        return Ok(String::from_utf8_lossy(&bytes[start..]).into_owned());
    }
    let len = f.metadata()?.len();
    let mut buf = Vec::new();
    if len <= cap {
        f.read_to_end(&mut buf)?;
        return Ok(String::from_utf8_lossy(&buf).into_owned());
    }
    f.seek(SeekFrom::Start(len - cap))?;
    f.take(cap).read_to_end(&mut buf)?;
    let start = buf
        .iter()
        .position(|b| *b == b'\n')
        .map_or(buf.len(), |i| i + 1);
    Ok(String::from_utf8_lossy(&buf[start..]).into_owned())
}

/// The agent's working directory, from the first few records (Claude records
/// and pi's session header both carry `cwd`).
fn find_cwd(text: &str) -> Option<PathBuf> {
    text.lines()
        .take(CWD_SCAN_LINES)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l.trim()).ok())
        .find_map(|v| v["cwd"].as_str().map(PathBuf::from))
}

type Sig = (u64, Option<SystemTime>);

enum Loaded {
    Unchanged,
    Missing,
    Unreadable,
    Entries {
        sig: Sig,
        cwd: Option<PathBuf>,
        entries: Vec<Entry>,
    },
}

fn load(path: &Path, kind: TranscriptKind, last: Option<Sig>, cwd: Option<PathBuf>) -> Loaded {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Loaded::Missing,
        Err(_) => return Loaded::Unreadable,
    };
    let sig = (meta.len(), meta.modified().ok());
    if last == Some(sig) {
        return Loaded::Unchanged;
    }
    let Ok(text) = read_capped(path, READ_CAP) else {
        return Loaded::Unreadable;
    };
    let cwd = cwd.or_else(|| find_cwd(&text));
    let entries = conversation::parse_entries(kind, &text, cwd.as_deref());
    Loaded::Entries { sig, cwd, entries }
}

struct State {
    window: Vec<Entry>,
    sig: Option<Sig>,
    cwd: Option<PathBuf>,
}

struct Inner {
    path: PathBuf,
    kind: TranscriptKind,
    state: Mutex<State>,
    tx: broadcast::Sender<Update>,
    #[cfg(test)]
    refreshes: std::sync::atomic::AtomicUsize,
}

impl Inner {
    async fn refresh(&self) {
        #[cfg(test)]
        self.refreshes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (last, cwd) = {
            let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (st.sig, st.cwd.clone())
        };
        let (path, kind) = (self.path.clone(), self.kind);
        let Ok(loaded) = tokio::task::spawn_blocking(move || load(&path, kind, last, cwd)).await
        else {
            return;
        };
        let (next, sig, new_cwd) = match loaded {
            Loaded::Unchanged => return,
            Loaded::Missing => (Vec::new(), None, None),
            Loaded::Unreadable => (vec![unreadable_notice()], None, None),
            Loaded::Entries { sig, cwd, entries } => (entries, Some(sig), cwd),
        };
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.sig = sig;
        if new_cwd.is_some() {
            st.cwd = new_cwd;
        }
        if let Some(update) = diff(&st.window, &next) {
            st.window = window(&next);
            // Sent under the lock so `subscribe` sees window and stream
            // consistently. No receivers is fine.
            let _ = self.tx.send(update);
        }
    }
}

fn unreadable_notice() -> Entry {
    Entry {
        id: "notice:unreadable".to_string(),
        kind: EntryKind::Notice,
        text: Some(UNREADABLE.to_string()),
        tool: None,
        duration_ms: None,
        at: None,
    }
}

/// One tailed transcript file, shared by all viewers of it. The background
/// task stops when the last `Arc<Tail>` drops.
///
/// Subscribers that fall more than the channel capacity behind get
/// `RecvError::Lagged`; the consumer must then call [`Tail::subscribe`] again
/// and start over from the fresh window.
pub struct Tail {
    inner: Arc<Inner>,
    task: JoinHandle<()>,
}

impl Tail {
    /// The current window and a receiver of every later update.
    pub fn subscribe(&self) -> (Vec<Entry>, broadcast::Receiver<Update>) {
        let st = self.inner.state.lock().unwrap_or_else(|e| e.into_inner());
        (st.window.clone(), self.inner.tx.subscribe())
    }

    /// The full result of tool call `tool_id` (re-reads the capped file).
    pub async fn tool_result(&self, tool_id: &str) -> io::Result<Option<ToolResult>> {
        let (path, kind, id) = (
            self.inner.path.clone(),
            self.inner.kind,
            tool_id.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let text = read_capped(&path, READ_CAP)?;
            Ok(conversation::tool_result(kind, &text, &id))
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl Drop for Tail {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One tail per transcript file, shared while anyone holds it.
#[derive(Clone, Default)]
pub struct Transcripts {
    map: Arc<tokio::sync::Mutex<HashMap<PathBuf, Weak<Tail>>>>,
}

impl Transcripts {
    pub fn new() -> Transcripts {
        Transcripts::default()
    }

    /// The live tail for `path`, starting one (with its first read done) if
    /// none is held. Must be called inside a tokio runtime.
    pub async fn get(&self, path: PathBuf, kind: TranscriptKind) -> Arc<Tail> {
        let mut map = self.map.lock().await;
        if let Some(tail) = map.get(&path).and_then(Weak::upgrade) {
            if tail.inner.kind == kind {
                return tail;
            }
        }
        map.retain(|_, w| w.strong_count() > 0);
        let (tx, _) = broadcast::channel(CHANNEL_CAP);
        let inner = Arc::new(Inner {
            path: path.clone(),
            kind,
            state: Mutex::new(State {
                window: Vec::new(),
                sig: None,
                cwd: None,
            }),
            tx,
            #[cfg(test)]
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        });
        inner.refresh().await;
        let wake = Arc::new(Notify::new());
        let watcher = watch(&path, wake.clone());
        let task = tokio::spawn(run(Arc::downgrade(&inner), wake, watcher));
        let tail = Arc::new(Tail { inner, task });
        map.insert(path, Arc::downgrade(&tail));
        tail
    }
}

/// Watch the parent directory (so create/replace are seen), filtered to the
/// file name. `None` if it can't be watched; the poll backstop still works.
fn watch(path: &Path, wake: Arc<Notify>) -> Option<notify::RecommendedWatcher> {
    let name = path.file_name()?.to_owned();
    let dir = path.parent()?;
    let mut w = notify::recommended_watcher(move |res: Result<Event, notify::Error>| {
        if let Ok(ev) = res {
            if ev
                .paths
                .iter()
                .any(|p| p.file_name() == Some(name.as_os_str()))
            {
                wake.notify_one();
            }
        }
    })
    .ok()?;
    w.watch(dir, RecursiveMode::NonRecursive).ok()?;
    Some(w)
}

async fn run(inner: Weak<Inner>, wake: Arc<Notify>, _watcher: Option<notify::RecommendedWatcher>) {
    loop {
        tokio::select! {
            _ = wake.notified() => tokio::time::sleep(COALESCE).await,
            _ = tokio::time::sleep(POLL) => {}
        }
        let Some(inner) = inner.upgrade() else {
            return;
        };
        inner.refresh().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{ToolLine, ToolStatus};
    use std::io::Write;
    use tempfile::TempDir;

    fn msg(id: &str) -> Entry {
        Entry {
            id: id.to_string(),
            kind: EntryKind::Assistant,
            text: Some(id.to_string()),
            tool: None,
            duration_ms: None,
            at: None,
        }
    }

    fn tool(id: &str, status: ToolStatus) -> Entry {
        Entry {
            id: id.to_string(),
            kind: EntryKind::Tool,
            text: None,
            tool: Some(ToolLine {
                id: id.to_string(),
                name: "Read".to_string(),
                summary: String::new(),
                status,
            }),
            duration_ms: None,
            at: None,
        }
    }

    #[test]
    fn diff_equal_is_none() {
        let a = vec![msg("A")];
        assert_eq!(diff(&a, &a), None);
    }

    #[test]
    fn diff_upsert_on_append_and_status_change() {
        let prev = vec![msg("A"), tool("T", ToolStatus::Pending)];
        let next = vec![msg("A"), tool("T", ToolStatus::Ok), msg("B")];
        assert_eq!(
            diff(&prev, &next),
            Some(Update::Upsert(vec![tool("T", ToolStatus::Ok), msg("B")]))
        );
    }

    #[test]
    fn fork_after_send_resets() {
        let prev = vec![msg("A"), msg("B")];
        let next = vec![msg("A"), msg("C")];
        assert_eq!(diff(&prev, &next), Some(Update::Replace(next.clone())));
    }

    #[test]
    fn window_is_last_200() {
        let next: Vec<Entry> = (0..250).map(|i| msg(&format!("e{i}"))).collect();
        match diff(&[], &next) {
            Some(Update::Replace(w)) => {
                assert_eq!(w.len(), 200);
                assert_eq!(w[0].id, "e50");
                assert_eq!(w[199].id, "e249");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn sliding_window_upserts_instead_of_replacing() {
        let all: Vec<Entry> = (0..201).map(|i| msg(&format!("e{i}"))).collect();
        let prev = window(&all[..200]);
        assert_eq!(diff(&prev, &all), Some(Update::Upsert(vec![msg("e200")])));
    }

    #[test]
    fn emptied_file_replaces_with_empty() {
        assert_eq!(diff(&[msg("A")], &[]), Some(Update::Replace(vec![])));
    }

    #[test]
    fn read_capped_skips_partial_first_line() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        fs::write(&p, "aaaa\nbb\ncc\n").unwrap();
        // 9 bytes = "aa\nbb\ncc\n": the partial "aa" line is dropped.
        assert_eq!(read_capped(&p, 9).unwrap(), "bb\ncc\n");
        assert_eq!(read_capped(&p, 100).unwrap(), "aaaa\nbb\ncc\n");
        assert_eq!(read_capped(&p, 6).unwrap(), "cc\n");
    }

    fn user(uuid: &str, parent: Option<&str>, text: &str) -> String {
        let parent = parent.map_or("null".to_string(), |p| format!("\"{p}\""));
        format!(
            r#"{{"type":"user","uuid":"{uuid}","parentUuid":{parent},"cwd":"/repo","message":{{"content":"{text}"}}}}"#
        )
    }

    fn append(p: &Path, line: &str) {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .unwrap();
        writeln!(f, "{line}").unwrap();
    }

    async fn next_update(rx: &mut broadcast::Receiver<Update>) -> Update {
        tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("no update within 2s")
            .unwrap()
    }

    #[tokio::test]
    async fn tail_broadcasts_appended_lines() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u1", None, "one"));
        append(&p, &user("u2", Some("u1"), "two"));
        let tail = Transcripts::new()
            .get(p.clone(), TranscriptKind::ClaudeCode)
            .await;
        let (win, mut rx) = tail.subscribe();
        assert_eq!(win.len(), 2);
        append(&p, &user("u3", Some("u2"), "three"));
        match next_update(&mut rx).await {
            Update::Upsert(es) => {
                assert_eq!(es.len(), 1);
                assert_eq!(es[0].text.as_deref(), Some("three"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn short_window_front_drop_is_replace() {
        let prev = vec![msg("A"), msg("B"), msg("C")];
        let next = vec![msg("B"), msg("C")];
        assert_eq!(diff(&prev, &next), Some(Update::Replace(next.clone())));
    }

    #[tokio::test]
    async fn burst_of_appends_coalesces_refreshes() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u0", None, "m0"));
        let tail = Transcripts::new()
            .get(p.clone(), TranscriptKind::ClaudeCode)
            .await;
        let (_, mut rx) = tail.subscribe();
        let mut parent = "u0".to_string();
        for i in 1..=20 {
            let id = format!("u{i}");
            append(&p, &user(&id, Some(&parent), &format!("m{i}")));
            parent = id;
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let mut seen_last = false;
        while !seen_last {
            let es = match next_update(&mut rx).await {
                Update::Replace(es) | Update::Upsert(es) => es,
            };
            seen_last = es.iter().any(|e| e.text.as_deref() == Some("m20"));
        }
        let n = tail
            .inner
            .refreshes
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(n <= 6, "{n} refreshes for one burst");
    }

    #[tokio::test]
    async fn missing_file_then_created() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("later.jsonl");
        let tail = Transcripts::new()
            .get(p.clone(), TranscriptKind::ClaudeCode)
            .await;
        let (win, mut rx) = tail.subscribe();
        assert!(win.is_empty());
        append(&p, &user("u1", None, "hello"));
        match next_update(&mut rx).await {
            Update::Replace(es) | Update::Upsert(es) => {
                assert_eq!(es.len(), 1);
                assert_eq!(es[0].text.as_deref(), Some("hello"));
            }
        }
    }

    #[tokio::test]
    async fn replaced_file_resets_window() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u1", None, "one"));
        append(&p, &user("u2", Some("u1"), "two"));
        let tail = Transcripts::new()
            .get(p.clone(), TranscriptKind::ClaudeCode)
            .await;
        let (_, mut rx) = tail.subscribe();
        fs::write(&p, user("z1", None, "fresh") + "\n").unwrap();
        match next_update(&mut rx).await {
            Update::Replace(es) => {
                assert_eq!(es.len(), 1);
                assert_eq!(es[0].text.as_deref(), Some("fresh"));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn unreadable_file_shows_notice() {
        let d = TempDir::new().unwrap();
        // A directory where the file should be: metadata ok, read fails.
        let p = d.path().join("t.jsonl");
        fs::create_dir(&p).unwrap();
        let tail = Transcripts::new().get(p, TranscriptKind::ClaudeCode).await;
        let (win, _rx) = tail.subscribe();
        assert_eq!(win.len(), 1);
        assert_eq!(win[0].kind, EntryKind::Notice);
        assert_eq!(win[0].text.as_deref(), Some(UNREADABLE));
    }

    #[tokio::test]
    async fn cwd_from_records_relativizes_tool_paths() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u1", None, "go"));
        append(
            &p,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/repo/src/a.rs"}}]}}"#,
        );
        let tail = Transcripts::new().get(p, TranscriptKind::ClaudeCode).await;
        let (win, _rx) = tail.subscribe();
        let t = win.iter().find_map(|e| e.tool.as_ref()).unwrap();
        assert!(t.summary.contains("src/a.rs") && !t.summary.contains("/repo/"));
    }

    #[tokio::test]
    async fn tool_result_reads_full_result() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u1", None, "go"));
        append(
            &p,
            r#"{"type":"assistant","uuid":"a1","parentUuid":"u1","message":{"content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#,
        );
        append(
            &p,
            r#"{"type":"user","uuid":"u2","parentUuid":"a1","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"output!"}]}}"#,
        );
        let tail = Transcripts::new().get(p, TranscriptKind::ClaudeCode).await;
        let r = tail.tool_result("t1").await.unwrap().unwrap();
        assert_eq!(r.text, "output!");
        assert!(tail.tool_result("nope").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn tail_stops_when_dropped() {
        let d = TempDir::new().unwrap();
        let p = d.path().join("t.jsonl");
        append(&p, &user("u1", None, "one"));
        let ts = Transcripts::new();
        let a = ts.get(p.clone(), TranscriptKind::ClaudeCode).await;
        let b = ts.get(p.clone(), TranscriptKind::ClaudeCode).await;
        assert!(Arc::ptr_eq(&a, &b));
        let weak = Arc::downgrade(&a);
        drop(a);
        drop(b);
        assert!(weak.upgrade().is_none());
        let c = ts.get(p, TranscriptKind::ClaudeCode).await;
        assert_eq!(Arc::strong_count(&c), 1);
        assert_eq!(ts.map.lock().await.len(), 1);
    }
}
